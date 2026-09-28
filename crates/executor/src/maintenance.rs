//
// © 2026 PLOMID Technology Solutions
//
// PLOMID
// Platform for Modern Intelligence and Data
//
// Author: Sainath Sapa
// GitHub: https://github.com/sainathsapa
//
// Licensed under the Apache License, Version 2.0;
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//! Generic, deterministic maintenance coordination for the SQL layer.
//!
//! PLOMID materializes committed SQL state into immutable generations. That
//! work is expensive, so it must never run once per `INSERT`/`UPDATE`/`DELETE`.
//! This module owns the small amount of policy and accounting needed to decide
//! *when* a table is due, and nothing else.
//!
//! The design is deliberately generic rather than index-specific:
//!
//! ```text
//! committed DML
//!     ↓
//! record one committed mutation     (cheap integer increment, no I/O)
//!     ↓
//! policy says "not due" ──────────→ continue normally
//!     ↓ due
//! maintenance pass                  (the SQL layer materializes the table)
//! ```
//!
//! Index generation is not scheduled here. The maintenance pass publishes a
//! stable data generation and the existing one-authority index-generation
//! engine consumes it, so no second index scheduler exists.
//!
//! # Determinism
//!
//! The policy is driven by a committed-mutation counter that only advances on a
//! successful commit, counted in rows written (not statements): a bulk commit
//! of N rows accrues N units, a single-row commit accrues one. It uses no
//! wall-clock time, no byte counts, and no background thread, so a test can
//! drive it exactly by executing statements.
//!
//! # Default policy
//!
//! [`MaintenancePolicy::DEFAULT_COMMITTED_MUTATIONS_PER_GENERATION`] committed
//! row-mutations (64) accumulate before one table is considered due. The bound is a
//! configuration value, not a hidden constant: [`MaintenancePolicy::new`]
//! accepts any value, including `1` (materialize after every commit) for
//! systems that explicitly want that, and [`MaintenanceCoordinator`] exposes
//! the active value.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// Process-wide single-flight claims for table maintenance, keyed by the
/// claim key (`storage-root/database/table`, see [`maintenance_claim_key`]).
///
/// The claim is deliberately non-blocking: a session (or the background
/// worker) that cannot take it skips the pass instead of waiting for another
/// pass, so no connection can be stalled behind unrelated maintenance.
/// Claims are released by [`MaintenanceClaim`]'s `Drop`, so a failed pass
/// (or a panic) frees the table immediately.
static MAINTENANCE_CLAIMS: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();

/// Process-wide timestamps of the last completed maintenance pass, keyed by
/// the same claim key. Consulted before claiming: a table maintained within
/// the policy interval is skipped (its mutations stay accumulated for a
/// later pass), so a write burst cannot trigger a full materialization per
/// policy bound on the commit path of an unlucky OLTP write. A zero interval
/// (tests, explicit administrative drives) always allows the pass; explicit
/// `VACUUM` bypasses the coordinator entirely and is always fresh. A failed
/// pass records nothing, so the next commit retries immediately.
/// Last completed pass per claim key: completion time and pass duration.
/// The duration scales the burst gate (see [`effective_auto_interval`]): a
/// fixed interval lets multi-minute passes chain back-to-back under bulk
/// load, dominating the machine they were meant to protect.
static MAINTENANCE_LAST_PASS: OnceLock<
    Mutex<std::collections::HashMap<String, (std::time::Instant, std::time::Duration)>>,
> = OnceLock::new();

/// Builds the process-wide claim key for one table's maintenance slot.
///
/// Keyed by the storage root (not just the table name): the claim must be
/// shared by every session over the *same* database directory, and must NOT
/// couple sessions in different directories (independent tests, or distinct
/// databases).
#[must_use]
pub(crate) fn maintenance_claim_key(root: &std::path::Path, database: &str, table: &str) -> String {
    format!("{}/{}/{}", root.display(), database, table)
}

/// Returns true when an automatic pass for `key` may proceed under `policy`,
/// recording nothing. A zero interval always allows the pass (deterministic
/// tests); otherwise the pass proceeds only when no pass completed within
/// the interval.
pub(crate) fn auto_pass_allowed(key: &str, policy: &MaintenancePolicy) -> bool {
    let map = MAINTENANCE_LAST_PASS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let guard = match map.lock() {
        Ok(guard) => guard,
        Err(_) => return true,
    };
    match guard.get(key) {
        Some((last, duration)) => last.elapsed() >= effective_auto_interval(policy, *duration),
        None => true,
    }
}

/// Burst-gate interval for the next automatic pass: the configured floor,
/// stretched to a multiple of the last pass's own duration. A fixed floor
/// lets passes that take minutes chain continuously under bulk load (each
/// pass is instantly stale yet costs a full rebuild); scaling with measured
/// cost bounds maintenance duty cycle instead. Zero stays zero so
/// deterministic tests and explicit drives are unaffected.
pub(crate) fn effective_auto_interval(
    policy: &MaintenancePolicy,
    last_duration: std::time::Duration,
) -> std::time::Duration {
    let floor = policy.min_auto_interval();
    if floor.is_zero() {
        return floor;
    }
    floor.max(last_duration.saturating_mul(3))
}

/// Duration of the last completed pass for `key`, if any. Used to scale
/// burst-gate deferrals (see [`effective_auto_interval`]).
pub(crate) fn last_pass_duration(key: &str) -> std::time::Duration {
    MAINTENANCE_LAST_PASS
        .get()
        .and_then(|map| map.lock().ok())
        .and_then(|guard| guard.get(key).map(|(_, duration)| *duration))
        .unwrap_or_default()
}

/// Instant of the last completed pass for `key`, if any. Used to date
/// burst-gate deferrals: a denied pass requeues for the moment the interval
/// lapses rather than a fixed delay, so the deferral never drifts later
/// under repeated denials.
pub(crate) fn last_pass_at(key: &str) -> Option<std::time::Instant> {
    MAINTENANCE_LAST_PASS
        .get()?
        .lock()
        .ok()?
        .get(key)
        .map(|(at, _)| *at)
}

/// Records a completed pass for `key` (pass ran to success).
pub(crate) fn record_auto_pass(key: &str, duration: std::time::Duration) {
    let map = MAINTENANCE_LAST_PASS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut guard) = map.lock() {
        guard.insert(key.to_string(), (std::time::Instant::now(), duration));
    }
}

/// RAII guard for one claimed `(database, table)` maintenance slot.
pub(crate) struct MaintenanceClaim {
    key: String,
}

impl MaintenanceClaim {
    pub(crate) fn try_acquire(key: String) -> Option<Self> {
        let claims =
            MAINTENANCE_CLAIMS.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
        let mut claims = claims.lock().ok()?;
        if !claims.insert(key.clone()) {
            return None;
        }
        Some(Self { key })
    }

    /// Acquires `key`, waiting (bounded) while another pass holds it.
    ///
    /// Only explicit administrative work (`VACUUM`) waits: automatic passes
    /// keep skipping on conflict so no commit path ever stalls. Waiting is
    /// deadlock-free by construction — holders never wait (they run to
    /// completion and release via `Drop`, including on panic), and the waiter
    /// holds no engine locks while polling, so the holder's pass can always
    /// proceed. On timeout returns `None` so the caller fails loudly instead
    /// of running an uncoordinated pass into an allocator race.
    pub(crate) fn acquire_wait(key: String, timeout: std::time::Duration) -> Option<Self> {
        let started = std::time::Instant::now();
        loop {
            if let Some(claim) = Self::try_acquire(key.clone()) {
                return Some(claim);
            }
            if started.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl Drop for MaintenanceClaim {
    fn drop(&mut self) {
        if let Some(claims) = MAINTENANCE_CLAIMS.get() {
            if let Ok(mut claims) = claims.lock() {
                claims.remove(&self.key);
            }
        }
    }
}

/// Deterministic, configurable policy that decides when a table is due for
/// maintenance.
///
/// The single signal is the number of committed mutations accumulated for the
/// table since its last maintenance pass. That is a logical signal, so the
/// decision is reproducible and testable without sleeping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaintenancePolicy {
    committed_mutations_per_generation: u64,
    /// Minimum interval between two *automatic* maintenance passes for the
    /// same table (process-wide). Explicit maintenance (`VACUUM`,
    /// [`crate::Executor::run_maintenance`]) is never gated: it runs on
    /// demand. Zero disables gating entirely, which is what tests use so a
    /// policy is driven exactly by executing statements rather than by
    /// waiting.
    min_auto_interval: std::time::Duration,
}

impl MaintenancePolicy {
    /// Documented default: a table becomes due once 64 committed mutations have
    /// accumulated since its last maintained generation.
    ///
    /// A mutation is one committed `INSERT`/`UPDATE`/`DELETE`/`TRUNCATE`
    /// statement, counted once per affected table. The value is intentionally
    /// explicit so operators can reason about generation volume: a table under
    /// steady write load produces one immutable generation per 64 committed
    /// mutations rather than one per write.
    pub const DEFAULT_COMMITTED_MUTATIONS_PER_GENERATION: u64 = 64;

    /// Minimum interval between automatic passes in the production server
    /// policy. A full-table materialization on the commit path of an unlucky
    /// OLTP write is the dominant p99 killer under write bursts (measured:
    /// 179 generations in one 1M-statement mixed run, ~90x rewrite
    /// amplification); gating bursts to one automatic pass per minute per
    /// table bounds that damage while explicit `VACUUM` stays ungated.
    pub const PRODUCTION_MIN_AUTO_INTERVAL: std::time::Duration =
        std::time::Duration::from_secs(60);

    /// Builds a policy that makes a table due after `n` committed mutations.
    ///
    /// A value of `0` is treated as `1`: a table is always allowed to become due
    /// after a single commit, which is the most eager valid policy. Automatic
    /// passes are ungated (interval zero) so tests drive maintenance exactly
    /// by executing statements.
    #[must_use]
    pub fn new(committed_mutations_per_generation: u64) -> Self {
        Self {
            committed_mutations_per_generation: committed_mutations_per_generation.max(1),
            min_auto_interval: std::time::Duration::ZERO,
        }
    }

    /// Production server policy: the default mutation bound plus a minimum
    /// interval between automatic passes for the same table.
    #[must_use]
    pub fn production() -> Self {
        Self {
            committed_mutations_per_generation: Self::DEFAULT_COMMITTED_MUTATIONS_PER_GENERATION,
            min_auto_interval: Self::PRODUCTION_MIN_AUTO_INTERVAL,
        }
    }

    /// Overrides the minimum automatic-pass interval, returning the policy.
    ///
    /// Tests use this to opt into gating explicitly without changing the
    /// mutation bound.
    #[must_use]
    pub fn with_min_auto_interval(mut self, interval: std::time::Duration) -> Self {
        self.min_auto_interval = interval;
        self
    }

    /// The configured mutation bound.
    #[must_use]
    pub fn committed_mutations_per_generation(&self) -> u64 {
        self.committed_mutations_per_generation
    }

    /// The minimum interval between automatic passes for one table.
    #[must_use]
    pub fn min_auto_interval(&self) -> std::time::Duration {
        self.min_auto_interval
    }

    /// True when `committed_mutations` has reached the configured bound.
    #[must_use]
    pub fn is_due(&self, committed_mutations: u64) -> bool {
        committed_mutations >= self.committed_mutations_per_generation
    }
}

impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self::new(Self::DEFAULT_COMMITTED_MUTATIONS_PER_GENERATION)
    }
}

/// Committed-mutation accounting for one table.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TableMaintenanceState {
    committed_mutations: u64,
    maintenance_passes: u64,
}

impl TableMaintenanceState {
    /// Mutations committed since the last maintained generation.
    #[must_use]
    pub fn committed_mutations(&self) -> u64 {
        self.committed_mutations
    }

    /// Completed maintenance passes for this table.
    #[must_use]
    pub fn maintenance_passes(&self) -> u64 {
        self.maintenance_passes
    }

    /// Records one committed mutation.
    ///
    /// This is the only work an ordinary write contributes to maintenance: an
    /// in-memory increment. No generation is built, no index is touched, and no
    /// I/O happens here.
    pub fn record_committed_mutation(&mut self) {
        self.record_committed_mutations(1);
    }

    /// Records `rows` committed mutations (zero allowed: a write that
    /// changed nothing accrues no debt).
    pub fn record_committed_mutations(&mut self, rows: u64) {
        self.committed_mutations = self.committed_mutations.saturating_add(rows);
    }

    /// Marks one maintenance pass as having consumed the accumulated mutations.
    fn record_maintenance_pass(&mut self) {
        self.committed_mutations = 0;
        self.maintenance_passes = self.maintenance_passes.saturating_add(1);
    }
}

/// Per-session maintenance accounting across the tables a session has written.
#[derive(Clone, Debug)]
pub struct MaintenanceCoordinator {
    policy: MaintenancePolicy,
    tables: BTreeMap<String, TableMaintenanceState>,
}

impl Default for MaintenanceCoordinator {
    fn default() -> Self {
        Self::new(MaintenancePolicy::default())
    }
}

impl MaintenanceCoordinator {
    /// Builds a coordinator using `policy`.
    #[must_use]
    pub fn new(policy: MaintenancePolicy) -> Self {
        Self {
            policy,
            tables: BTreeMap::new(),
        }
    }

    /// The active policy.
    #[must_use]
    pub fn policy(&self) -> &MaintenancePolicy {
        &self.policy
    }

    /// Replaces the active policy.
    ///
    /// The accounting already recorded is kept, so tightening or relaxing the
    /// bound takes effect on the next evaluation without losing committed work.
    pub fn set_policy(&mut self, policy: MaintenancePolicy) {
        self.policy = policy;
    }

    /// Records `rows` committed mutations for `table` and reports whether
    /// the table is now due.
    ///
    /// Debt is counted in rows written, not statements: a 5,000-row bulk
    /// commit and a single-row OLTP commit must not weigh the same, or bulk
    /// loads never become due (no automatic freshness, ever) while tiny
    /// commits each count a full unit toward a whole-table rebuild.
    pub fn record_committed_mutations(&mut self, table: &str, rows: u64) -> bool {
        let state = self.tables.entry(table.to_string()).or_default();
        state.record_committed_mutations(rows);
        self.policy.is_due(state.committed_mutations())
    }

    /// Records one committed mutation for `table` and reports whether the table
    /// is now due. Used where the row count is unknown (explicit-transaction
    /// groups report one result per group); statement-level callers with
    /// known counts must use [`Self::record_committed_mutations`].
    pub fn record_committed_mutation(&mut self, table: &str) -> bool {
        self.record_committed_mutations(table, 1)
    }

    /// Committed mutations accumulated for `table`.
    #[must_use]
    pub fn committed_mutations(&self, table: &str) -> u64 {
        self.tables
            .get(table)
            .map_or(0, TableMaintenanceState::committed_mutations)
    }

    /// Completed maintenance passes for `table`.
    #[must_use]
    pub fn maintenance_passes(&self, table: &str) -> u64 {
        self.tables
            .get(table)
            .map_or(0, TableMaintenanceState::maintenance_passes)
    }

    /// Tables whose accumulated mutations have reached the policy bound.
    ///
    /// Returned in deterministic (lexicographic) order so a maintenance pass
    /// behaves identically regardless of write order.
    #[must_use]
    pub fn due_tables(&self) -> Vec<String> {
        self.tables
            .iter()
            .filter(|(_, state)| self.policy.is_due(state.committed_mutations()))
            .map(|(table, _)| table.clone())
            .collect()
    }

    /// True when at least one table is due.
    #[must_use]
    pub fn any_due(&self) -> bool {
        self.tables
            .values()
            .any(|state| self.policy.is_due(state.committed_mutations()))
    }

    /// Marks `table` as maintained, consuming its accumulated mutations.
    ///
    /// Called only after a table's maintenance pass succeeded, so a failed pass
    /// leaves the mutations recorded and a later pass retries against them.
    pub fn record_maintenance_pass(&mut self, table: &str) {
        if let Some(state) = self.tables.get_mut(table) {
            state.record_maintenance_pass();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_documented_and_configurable() {
        assert_eq!(
            MaintenancePolicy::default().committed_mutations_per_generation(),
            MaintenancePolicy::DEFAULT_COMMITTED_MUTATIONS_PER_GENERATION
        );
        assert_eq!(
            MaintenancePolicy::new(7).committed_mutations_per_generation(),
            7
        );
        // Zero is clamped to the most eager valid policy rather than disabled.
        assert_eq!(
            MaintenancePolicy::new(0).committed_mutations_per_generation(),
            1
        );
    }

    #[test]
    fn writes_accumulate_and_only_become_due_at_the_bound() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(3));
        assert!(!coordinator.record_committed_mutation("public.t"));
        assert!(!coordinator.record_committed_mutation("public.t"));
        assert_eq!(coordinator.committed_mutations("public.t"), 2);
        assert!(coordinator.due_tables().is_empty());

        // The third commit reaches the bound: the table is due, and the
        // accounting still shows all three committed mutations.
        assert!(coordinator.record_committed_mutation("public.t"));
        assert_eq!(coordinator.due_tables(), vec!["public.t".to_string()]);
        assert_eq!(coordinator.committed_mutations("public.t"), 3);
    }

    #[test]
    fn a_pass_consumes_the_accumulated_mutations() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(2));
        coordinator.record_committed_mutation("t");
        coordinator.record_committed_mutation("t");
        assert!(coordinator.any_due());
        coordinator.record_maintenance_pass("t");
        assert_eq!(coordinator.committed_mutations("t"), 0);
        assert_eq!(coordinator.maintenance_passes("t"), 1);
        assert!(!coordinator.any_due());
    }

    #[test]
    fn tables_are_accounted_and_ordered_independently() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(1));
        coordinator.record_committed_mutation("b");
        coordinator.record_committed_mutation("a");
        coordinator.record_committed_mutation("b");
        // Deterministic order, and "a" stays due only once regardless of count.
        assert_eq!(
            coordinator.due_tables(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(coordinator.committed_mutations("a"), 1);
        assert_eq!(coordinator.committed_mutations("b"), 2);
    }

    #[test]
    fn changing_the_policy_preserves_recorded_work() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(10));
        coordinator.record_committed_mutation("t");
        assert!(!coordinator.any_due());
        coordinator.set_policy(MaintenancePolicy::new(1));
        assert!(coordinator.any_due());
        assert_eq!(coordinator.committed_mutations("t"), 1);
    }
}

#[cfg(test)]
mod row_count_tests {
    use super::*;

    #[test]
    fn bulk_commit_trips_the_bound_in_one_statement() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(64));
        // A single 5,000-row bulk commit accrues row debt, not one unit.
        assert!(coordinator.record_committed_mutations("public.t", 5000));
        assert_eq!(coordinator.committed_mutations("public.t"), 5000);
        assert_eq!(coordinator.due_tables(), vec!["public.t".to_string()]);
    }

    #[test]
    fn single_row_commits_accrue_exactly_as_before() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(3));
        assert!(!coordinator.record_committed_mutations("public.t", 1));
        assert!(!coordinator.record_committed_mutations("public.t", 1));
        assert!(coordinator.record_committed_mutations("public.t", 1));
        assert_eq!(coordinator.committed_mutations("public.t"), 3);
    }

    #[test]
    fn empty_writes_accrue_no_debt() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::new(1));
        assert!(!coordinator.record_committed_mutations("public.t", 0));
        assert!(coordinator.due_tables().is_empty());
    }
}

#[cfg(test)]
mod auto_interval_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn zero_interval_stays_zero() {
        let policy = MaintenancePolicy::new(1);
        assert!(policy.min_auto_interval().is_zero());
        assert!(effective_auto_interval(&policy, Duration::from_secs(3600)).is_zero());
    }

    #[test]
    fn interval_scales_with_pass_duration() {
        let policy = MaintenancePolicy::production();
        assert_eq!(
            effective_auto_interval(&policy, Duration::from_secs(1)),
            Duration::from_secs(60)
        );
        assert_eq!(
            effective_auto_interval(&policy, Duration::from_secs(100)),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn gate_denies_within_scaled_window() {
        let mut coordinator = MaintenanceCoordinator::new(MaintenancePolicy::production());
        let key = "public.t";
        // A 100s pass just completed: the 60s floor stretches to 300s.
        coordinator.record_committed_mutations(key, 10_000);
        assert!(coordinator.due_tables().contains(&key.to_string()));
        // Simulate the completed pass by recording directly.
        record_auto_pass(key, Duration::from_secs(100));
        assert!(!auto_pass_allowed(key, &MaintenancePolicy::production()));
        assert_eq!(last_pass_duration(key), Duration::from_secs(100));
    }
}
