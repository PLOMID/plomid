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
//! Shared columnar freshness tracking.
//!
//! A published columnar generation covers committed rows up to the snapshot
//! its build observed. Any later committed write to the table makes that
//! generation stale for new statements, until the next successful
//! publication. This module tracks that property process-wide so every
//! session — including connections that never ran `VACUUM` themselves —
//! agrees on whether a table's published generation is safe to read.
//!
//! # Design
//!
//! The map holds one entry per `(storage root, database, resolved table)`:
//!
//! ```text
//! writes: commits observed since the entry was created
//! clean:  the generation proven to cover every committed write, if any
//! ```
//!
//! * Any statement that may write table rows bumps `writes` and clears
//!   `clean` **before** it executes, so concurrent readers can never observe
//!   a generation that misses a committed write (the mark precedes
//!   visibility; rollbacks stay dirty conservatively until the next vacuum).
//! * A successful publication records `clean` only when `writes` still equals
//!   the value read before the build scan started: a commit landing anywhere
//!   inside the build window is observed and the table stays on the row path.
//! * The map starts empty at process boot, which fails closed: no entry means
//!   "not proven fresh", so readers use Hot Row Store state until the first
//!   post-boot publication. No recovery-time reconstruction is needed.
//! * Table drops remove the entry, so a recreated table with reused storage
//!   identities can never inherit another table's freshness.
//!
//! All mutations hold one process-wide mutex, and the check-and-set in
//! [`try_mark_published`] is atomic under it, so the interleavings above hold
//! across concurrent sessions. Critical sections only hash short strings.

use plomid_core::GenerationId;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Freshness state of one table's published generation.
#[derive(Clone, Copy, Debug)]
struct TableFreshness {
    /// Committed write statements observed since process boot (or entry
    /// creation). Monotonic per table; only compared for equality.
    writes: u64,
    /// Generation proven to cover every committed write, if any.
    clean: Option<GenerationId>,
}

/// Process-wide freshness map, keyed by `storage-root/database/table`.
fn map() -> &'static Mutex<HashMap<String, TableFreshness>> {
    static FRESHNESS: OnceLock<Mutex<HashMap<String, TableFreshness>>> = OnceLock::new();
    FRESHNESS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Storage key shared by writers, publishers, and readers.
///
/// Uses the same `root/database/table` spelling as the maintenance claim
/// keys so one session's writes invalidate exactly the entries every other
/// session reads.
#[must_use]
pub(crate) fn freshness_key(root: &std::path::Path, database: &str, table: &str) -> String {
    format!("{}/{}/{}", root.display(), database, table)
}

/// Marks `table` dirty before a statement that may write its rows.
///
/// Unconditional: even statements that later fail or roll back leave the
/// entry dirty, which only costs a row-path fallback until the next vacuum.
pub(crate) fn mark_table_dirty(key: &str) {
    if let Ok(mut guard) = map().lock() {
        let entry = guard.entry(key.to_string()).or_insert(TableFreshness {
            writes: 0,
            clean: None,
        });
        entry.writes = entry.writes.saturating_add(1);
        entry.clean = None;
    }
}

/// Reads the current write count for `key` (0 when absent).
pub(crate) fn writes_since_boot(key: &str) -> u64 {
    map()
        .lock()
        .ok()
        .and_then(|guard| guard.get(key).map(|entry| entry.writes))
        .unwrap_or(0)
}

/// Records a freshly published `generation` as clean, but only when no write
/// landed since `writes_before` was read (i.e. the build window observed no
/// concurrent commit). Returns whether the table is now marked clean.
pub(crate) fn try_mark_published(key: &str, generation: GenerationId, writes_before: u64) -> bool {
    let Ok(mut guard) = map().lock() else {
        return false;
    };
    match guard.get(key) {
        // No commits ever observed through this path (bootstrap/recovered
        // rows only): the just-published generation covers everything the
        // build scan saw, and no concurrent commit could have interleaved
        // without creating an entry.
        None => {
            guard.insert(
                key.to_string(),
                TableFreshness {
                    writes: 0,
                    clean: Some(generation),
                },
            );
            true
        }
        Some(entry) if entry.writes == writes_before => {
            let entry = guard.get_mut(key).expect("entry checked above");
            entry.clean = Some(generation);
            true
        }
        // A commit landed inside the build window: stay on the row path.
        Some(_) => false,
    }
}

/// Returns the clean generation for `key`, if one is currently proven fresh.
pub(crate) fn clean_generation(key: &str) -> Option<GenerationId> {
    map()
        .lock()
        .ok()
        .and_then(|guard| guard.get(key).and_then(|entry| entry.clean))
}

/// Returns true when `table`'s published generation is proven to cover every
/// committed write visible to new statements.
///
/// Both halves are required: the shared map must hold a clean generation for
/// `(root, database, table)`, and that generation must still be the
/// currently published one. Any failure — missing entry, unreadable
/// publication state, unpublished object, superseded generation — answers
/// false and the caller uses Hot Row Store state. Fresh connections therefore
/// discover columnar eligibility from authoritative database state without
/// ever having run `VACUUM` themselves.
pub(crate) fn is_generation_fresh(
    store: &plomid_columnar::ColumnarStore,
    root: &std::path::Path,
    database: &str,
    resolved_table: &str,
    table_id: u64,
) -> bool {
    let key = freshness_key(root, database, resolved_table);
    let Some(clean) = clean_generation(&key) else {
        return false;
    };
    let Ok(snapshot) = store.generations().reader() else {
        return false;
    };
    let object = plomid_core::ObjectId::new(table_id);
    let Some(record) = snapshot.object(object) else {
        return false;
    };
    record.is_published() && record.current_generation == clean
}

/// Removes all freshness state for `key` (table/index drops and renames).
pub(crate) fn forget_table(key: &str) {
    if let Ok(mut guard) = map().lock() {
        guard.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(table: &str) -> String {
        freshness_key(std::path::Path::new("/root"), "db", table)
    }

    #[test]
    fn absent_table_is_not_fresh() {
        let key = key("never-written");
        forget_table(&key);
        assert_eq!(clean_generation(&key), None);
        assert_eq!(writes_since_boot(&key), 0);
    }

    #[test]
    fn dirty_write_clears_freshness() {
        let key = key("dirty-write");
        forget_table(&key);
        assert!(try_mark_published(&key, GenerationId::new(7), 0));
        assert_eq!(clean_generation(&key), Some(GenerationId::new(7)));
        mark_table_dirty(&key);
        assert_eq!(clean_generation(&key), None);
        assert_eq!(writes_since_boot(&key), 1);
        forget_table(&key);
    }

    #[test]
    fn concurrent_write_inside_build_window_blocks_marking() {
        let key = key("build-race");
        forget_table(&key);
        mark_table_dirty(&key);
        mark_table_dirty(&key);
        let writes_before = writes_since_boot(&key);
        assert_eq!(writes_before, 2);
        // A commit landing now must defeat the mark even though the check
        // runs after it.
        mark_table_dirty(&key);
        assert!(!try_mark_published(
            &key,
            GenerationId::new(9),
            writes_before
        ));
        assert_eq!(clean_generation(&key), None);
        // A clean build window marks exactly the published generation.
        let writes_before = writes_since_boot(&key);
        assert!(try_mark_published(
            &key,
            GenerationId::new(10),
            writes_before
        ));
        assert_eq!(clean_generation(&key), Some(GenerationId::new(10)));
        forget_table(&key);
    }

    #[test]
    fn forget_drops_claims() {
        let key = key("dropped");
        forget_table(&key);
        assert!(try_mark_published(&key, GenerationId::new(3), 0));
        forget_table(&key);
        assert_eq!(clean_generation(&key), None);
    }
}
