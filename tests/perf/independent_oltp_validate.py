#!/usr/bin/env python3
"""Independent-row OLTP concurrency and group-commit validation.

Measurement-only harness. No production code is modified.

Workload A — independent single-row transactions:
    each of W workers owns one distinct row and runs `A_OPS` transactions,
    each `UPDATE walval.accounts SET value = value + 1 WHERE id = <own row>`.

Workload B — independent multi-row transactions:
    each of W workers owns a distinct range of `B_ROWS` rows and runs `B_OPS`
    transactions, each updating all of its rows inside one explicit
    BEGIN ... COMMIT (one commit, `B_ROWS` operations).

Both run in two connection modes (`reuse` = one connection per worker,
`reconnect` = one connection per transaction).

Each measurement cell boots a FRESH release `plomid-server` on its own
isolated data directory (`target/bench-independent-oltp/<cell>/`), creates
`walval.accounts` with 10000 rows, optionally fills the WAL with genuinely
retained segments, runs the workload, verifies every row, and records:

  * throughput and client transaction latency (p50/p95/p99),
  * the production `plomid::perf` stage timings (`txn_commit`,
    `commit_prepare`, `commit_publish`, `update_stmt`, `write_gate`,
    `storage_apply`, `mvcc_*`),
  * the `group_flush` group-size distribution and flush latency,
  * exact WAL fsync counts via a measurement-only DYLD interposer
    (`target/bench-walval/libsyncshim.dylib`),
  * server-side automatic-maintenance failures and connection rejections.

Environment overrides:
    CONFIGS      "A:1,B:1,A:5"    workload:retained-segments list
    W_SET        worker counts     (default 1,2,4,8,16,32)
    REPS         repetitions       (default 3)
    A_OPS        transactions/worker (default 100)
    B_OPS        transactions/worker (default 50)
    B_ROWS       rows per transaction (default 10)
    MODES        reuse,reconnect   (default both)
    EXTRA        rollback,restart  (default rollback,restart)
    RUN_TAG      cell-name tag     (default r1)
    RESULTS      JSON output path
"""

import json
import os
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import psycopg

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.getenv("PLOMID_BIN", os.path.join(ROOT, "target", "release", "plomid-server"))
SHIM = os.getenv(
    "SYNC_SHIM", os.path.join(ROOT, "target", "bench-walval", "libsyncshim.dylib")
)
DATA_ROOT = os.getenv(
    "DATA_ROOT", os.path.join(ROOT, "target", "bench-independent-oltp")
)
RUN_TAG = os.getenv("RUN_TAG", "r1")
RESULTS = os.getenv("RESULTS", os.path.join(DATA_ROOT, f"results-{RUN_TAG}.json"))

SCHEMA = "walval"
TABLE = "accounts"
ROWS_TOTAL = 10000
FILLER_VALUE_BYTES = 15360
FILLER_BATCH_ROWS = 64
USER = "plomid"
PASSWORD = "plomid"

CONFIGS = [c for c in os.getenv("CONFIGS", "A:1,B:1").split(",") if c]
W_SET = [int(w) for w in os.getenv("W_SET", "1,2,4,8,16,32").split(",")]
REPS = int(os.getenv("REPS", "3"))
A_OPS = int(os.getenv("A_OPS", "100"))
B_OPS = int(os.getenv("B_OPS", "50"))
B_ROWS = int(os.getenv("B_ROWS", "10"))
MODES = [m for m in os.getenv("MODES", "reuse,reconnect").split(",") if m]
EXTRA = [e for e in os.getenv("EXTRA", "rollback,restart").split(",") if e]

FIELD_RE = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)=("([^"]*)"|[^\s]+)')

_base = 20000 + (os.getpid() % 400) * 100
_next_port = [_base]


def next_port():
    _next_port[0] += 1
    return _next_port[0]


def dsn(port):
    return f"postgresql://{USER}:{PASSWORD}@127.0.0.1:{port}/plomid"


# --------------------------------------------------------------------------
# server lifecycle
# --------------------------------------------------------------------------


class Server:
    def __init__(self, data_dir, port, sync_log):
        self.data_dir = data_dir
        self.port = port
        self.sync_log = sync_log
        self.log_path = data_dir.rstrip("/") + ".server.log"
        os.makedirs(data_dir, exist_ok=True)
        env = dict(os.environ)
        if os.path.exists(SHIM):
            env["DYLD_INSERT_LIBRARIES"] = SHIM
            env["PLOMID_SYNC_LOG"] = sync_log
        env["RUST_LOG"] = "warn,plomid=info"
        self._log = open(self.log_path, "a")
        self.proc = subprocess.Popen(
            [
                BIN,
                "--data", data_dir,
                "--host", "127.0.0.1",
                "--port", str(port),
                "--username", USER,
                "--password", PASSWORD,
                "--log-level", "warn",
            ],
            stdout=self._log,
            stderr=subprocess.STDOUT,
            env=env,
        )

    def wait_ready(self, timeout=120):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc.poll() is not None:
                tail = open(self.log_path).read()[-2000:]
                raise RuntimeError(f"server exited during startup:\n{tail}")
            with socket.socket() as s:
                s.settimeout(0.2)
                if s.connect_ex(("127.0.0.1", self.port)) == 0:
                    return
            time.sleep(0.05)
        raise RuntimeError("server did not become ready")

    def stop(self, timeout=180):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGINT)
            try:
                self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=10)
        try:
            self._log.close()
        except ValueError:
            pass

    def log_size(self):
        return os.path.getsize(self.log_path)


def connect(port, autocommit=True):
    return psycopg.connect(dsn(port), autocommit=autocommit, connect_timeout=15)


# --------------------------------------------------------------------------
# data setup
# --------------------------------------------------------------------------


def wal_listing(data_dir):
    d = os.path.join(data_dir, "wal")
    out = []
    for name in sorted(os.listdir(d)):
        p = os.path.join(d, name)
        if os.path.isfile(p) and name.startswith("WAL-") and name.endswith(".dat"):
            out.append((name, os.path.getsize(p)))
    return out


def create_accounts(port):
    with connect(port) as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"CREATE TABLE {SCHEMA}.{TABLE} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL)"
        )
        step = 1000
        for start in range(1, ROWS_TOTAL + 1, step):
            end = min(start + step, ROWS_TOTAL + 1)
            values = ",".join(f"({i},0)" for i in range(start, end))
            cur.execute(f"INSERT INTO {SCHEMA}.{TABLE} (id, value) VALUES {values}")


def fill_wal(port, data_dir, target):
    value = "x" * FILLER_VALUE_BYTES
    next_id = 1000
    if target > 1:
        with connect(port) as conn:
            conn.cursor().execute(
                f"CREATE TABLE IF NOT EXISTS {SCHEMA}.filler "
                "(id BIGINT PRIMARY KEY, v TEXT NOT NULL)"
            )
    while len(wal_listing(data_dir)) < target:
        rows = []
        for _ in range(FILLER_BATCH_ROWS):
            next_id += 1
            rows.append(f"({next_id},'{value}')")
        with connect(port) as conn:
            conn.cursor().execute(
                f"INSERT INTO {SCHEMA}.filler (id, v) VALUES {','.join(rows)}"
            )
        if len(wal_listing(data_dir)) > target:
            raise RuntimeError(f"filler overshot {target}: {wal_listing(data_dir)}")


def reset_owned(port, workers, rows_per_txn):
    hi = workers * rows_per_txn
    with connect(port) as conn:
        conn.cursor().execute(
            f"UPDATE {SCHEMA}.{TABLE} SET value = 0 WHERE id BETWEEN 1 AND {hi}"
        )


def verify_owned(port, workers, rows_per_txn, expected):
    hi = workers * rows_per_txn
    with connect(port) as conn:
        cur = conn.cursor()
        cur.execute(
            f"SELECT count(*) FROM {SCHEMA}.{TABLE} "
            f"WHERE id BETWEEN 1 AND {hi} AND value = {expected}"
        )
        owned_ok = cur.fetchone()[0]
        cur.execute(
            f"SELECT count(*) FROM {SCHEMA}.{TABLE} "
            f"WHERE id BETWEEN 1 AND {hi} AND value <> {expected}"
        )
        owned_wrong = cur.fetchone()[0]
        cur.execute(
            f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id > {hi} AND value <> 0"
        )
        untouched_nonzero = cur.fetchone()[0]
    return {
        "owned_correct": owned_ok,
        "owned_wrong": owned_wrong,
        "untouched_nonzero": untouched_nonzero,
        "ok": owned_ok == hi and owned_wrong == 0 and untouched_nonzero == 0,
    }


# --------------------------------------------------------------------------
# workloads
# --------------------------------------------------------------------------


def row_for(worker, rows_per_txn):
    base = worker * rows_per_txn + 1
    return list(range(base, base + rows_per_txn))


def run_workload(port, workers, mode, rows_per_txn, ops):
    latencies = []
    errors = []
    lock = threading.Lock()
    barrier = threading.Barrier(workers)

    def statements_for(worker):
        return [
            f"UPDATE {SCHEMA}.{TABLE} SET value = value + 1 WHERE id = {r}"
            for r in row_for(worker, rows_per_txn)
        ]

    def worker(wid):
        if mode == "reuse":
            conn = connect(port, autocommit=False)
            cur = conn.cursor()
            barrier.wait()
            local = []
            try:
                for _ in range(ops):
                    start = time.perf_counter()
                    for sql in statements_for(wid):
                        cur.execute(sql)
                    conn.commit()
                    local.append(time.perf_counter() - start)
            except Exception as exc:  # noqa: BLE001
                with lock:
                    errors.append(f"{type(exc).__name__}: {str(exc)[:120]}")
            finally:
                conn.close()
            with lock:
                latencies.extend(local)
            return
        barrier.wait()
        local = []
        for _ in range(ops):
            conn = None
            try:
                conn = connect(port, autocommit=False)
                cur = conn.cursor()
                start = time.perf_counter()
                for sql in statements_for(wid):
                    cur.execute(sql)
                conn.commit()
                local.append(time.perf_counter() - start)
            except Exception as exc:  # noqa: BLE001
                with lock:
                    errors.append(f"{type(exc).__name__}: {str(exc)[:120]}")
            finally:
                if conn is not None:
                    try:
                        conn.close()
                    except Exception:  # noqa: BLE001
                        pass
        with lock:
            latencies.extend(local)

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, w) for w in range(workers)]
        for future in futures:
            future.result()
    return time.perf_counter() - start, latencies, errors


# --------------------------------------------------------------------------
# perf / sync log parsing
# --------------------------------------------------------------------------


def parse_perf(log_path, start_offset):
    with open(log_path) as f:
        f.seek(start_offset)
        chunk = f.read()
    events = []
    for line in chunk.splitlines():
        if "plomid::perf" not in line:
            continue
        fields = {
            m.group(1): (m.group(3) if m.group(3) is not None else m.group(2))
            for m in FIELD_RE.finditer(line)
        }
        if "event" in fields:
            events.append((fields["event"], fields))
    return events


def stage_values(events, event, field):
    out = []
    for name, fields in events:
        if name != event:
            continue
        try:
            out.append(int(fields[field]))
        except (KeyError, ValueError):
            continue
    return out


def parse_sync_log(path):
    entries = []
    if not os.path.exists(path):
        return entries
    with open(path) as f:
        for line in f:
            parts = line.split(" ", 2)
            if len(parts) != 3 or parts[0] not in ("F_FULLFSYNC", "fsync"):
                continue
            try:
                float(parts[1])
            except ValueError:
                continue
            entries.append(parts[2].strip())
    return entries


def wal_syncs_since(entries, start, end):
    count = 0
    files = {}
    for path in entries[start:end]:
        if "/wal/" not in path:
            continue
        count += 1
        files[path] = files.get(path, 0) + 1
    return count, files


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * fraction))]


def hist(values):
    out = {}
    for v in values:
        out[v] = out.get(v, 0) + 1
    return out


def count_lines(path, needle):
    if not os.path.exists(path):
        return 0
    n = 0
    with open(path) as f:
        for line in f:
            if needle in line:
                n += 1
    return n


# --------------------------------------------------------------------------
# measurement cell
# --------------------------------------------------------------------------


def boot_cell(kind, workload, segments, mode, workers, rows_per_txn):
    tag = f"{kind}-seg{segments}-{mode}-W{workers}-{RUN_TAG}"
    root = os.path.join(DATA_ROOT, tag)
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)
    data_dir = os.path.join(root, "data")
    sync_log = os.path.join(root, "sync.log")
    server = Server(data_dir, next_port(), sync_log)
    return root, data_dir, sync_log, server


def measure_workload_cell(workload, segments, mode, workers):
    rows_per_txn = 1 if workload == "A" else B_ROWS
    ops = A_OPS if workload == "A" else B_OPS
    root, data_dir, sync_log, server = boot_cell(
        f"{workload}", workload, segments, mode, workers, rows_per_txn
    )
    cell = {
        "kind": f"workload{workload}",
        "workload": workload,
        "segments_requested": segments,
        "mode": mode,
        "workers": workers,
        "rows_per_transaction": rows_per_txn,
        "transactions_per_worker": ops,
        "reps": [],
        "failed": False,
    }
    port = server.port
    try:
        server.wait_ready()
        create_accounts(port)
        fill_wal(port, data_dir, segments)
        listing = wal_listing(data_dir)
        cell["verified_files"] = [n for n, _ in listing]
        cell["verified_file_count"] = len(listing)
        cell["active_segment"] = listing[-1][0]
        if len(listing) != segments:
            raise RuntimeError(f"expected {segments} segments, found {listing}")

        for rep in range(REPS):
            reset_owned(port, workers, rows_per_txn)
            sync_start = len(parse_sync_log(sync_log))
            log_pos = server.log_size()
            wall, latencies, errors = run_workload(
                port, workers, mode, rows_per_txn, ops
            )
            time.sleep(0.1)
            sync_end = len(parse_sync_log(sync_log))
            events = parse_perf(server.log_path, log_pos)
            try:
                check = verify_owned(port, workers, rows_per_txn, ops)
            except Exception as exc:  # noqa: BLE001
                check = {"ok": False, "verify_error": f"{type(exc).__name__}: {exc}"}
            wal_syncs, files = wal_syncs_since(
                parse_sync_log(sync_log), sync_start, sync_end
            )
            groups = stage_values(events, "group_flush", "group")
            flush_us = stage_values(events, "group_flush", "flush_us")
            latency_ms = [v * 1000 for v in latencies]
            txns = workers * ops
            cell["reps"].append(
                {
                    "wall": wall,
                    "transactions": txns,
                    "completed_transactions": len(latencies),
                    "tx_per_sec": txns / wall,
                    "p50_ms": percentile(latency_ms, 0.50),
                    "p95_ms": percentile(latency_ms, 0.95),
                    "p99_ms": percentile(latency_ms, 0.99),
                    "errors": errors,
                    "wal_syncs": wal_syncs,
                    "wal_files_synced": files,
                    "rounds": len(groups),
                    "groups": groups,
                    "flush_us": flush_us,
                    "syncs_per_round": (
                        round(wal_syncs / len(groups), 3) if groups else None
                    ),
                    "txn_commit_mutex_wait_us": stage_values(
                        events, "txn_commit", "mutex_wait_us"
                    ),
                    "txn_commit_durability_wait_us": stage_values(
                        events, "txn_commit", "durability_wait_us"
                    ),
                    "txn_commit_publish_mutex_wait_us": stage_values(
                        events, "txn_commit", "publish_mutex_wait_us"
                    ),
                    "txn_commit_prepare_hold_us": stage_values(
                        events, "txn_commit", "prepare_hold_us"
                    ),
                    "txn_commit_publish_hold_us": stage_values(
                        events, "txn_commit", "publish_hold_us"
                    ),
                    "commit_prepare_wal_append_us": stage_values(
                        events, "commit_prepare", "wal_append_us"
                    ),
                    "commit_publish_apply_us": stage_values(
                        events, "commit_publish", "apply_us"
                    ),
                    "storage_apply_us": stage_values(
                        events, "storage_apply", "total_us"
                    ),
                    "write_gate_row_wait_us": [
                        int(f["wait_us"])
                        for n, f in events
                        if n == "write_gate" and f.get("kind") == "row"
                    ],
                    "write_gate_table_wait_us": [
                        int(f["wait_us"])
                        for n, f in events
                        if n == "write_gate" and f.get("kind") == "table"
                    ],
                    "update_stmt_lock_us": stage_values(
                        events, "update_stmt", "lock_us"
                    ),
                    "check": check,
                }
            )
            print(
                f"  {cell['kind']:<9} seg{segments} {mode:<9} W={workers:>2} "
                f"rep={rep + 1} wall={wall:7.3f}s tx/s={txns / wall:8.1f} "
                f"p50={percentile(latency_ms, 0.5):7.2f}ms "
                f"rounds={len(groups):>5} "
                f"gt1={sum(1 for g in groups if g > 1):>5} "
                f"maxg={max(groups) if groups else 0:>3} "
                f"syncs={wal_syncs:>5} ok={check['ok']} errs={len(errors)}",
                flush=True,
            )
            if errors or not check["ok"]:
                cell["failed"] = True
                print(f"      errors={errors[:2]} check={check}", flush=True)
                break
    except Exception as exc:  # noqa: BLE001
        cell["failed"] = True
        cell["cell_error"] = f"{type(exc).__name__}: {exc}"
        print(f"      CELL ERROR: {cell['cell_error']}", flush=True)
    finally:
        server.stop()
    cell["server_alive_at_end"] = server.proc.returncode in (0, None)
    cell["server_returncode"] = server.proc.returncode
    cell["maintenance_failed"] = count_lines(
        server.log_path, "automatic maintenance failed"
    )
    cell["catalog_generation_missing"] = count_lines(
        server.log_path, "generation metadata is missing"
    )
    cell["recovery_errors"] = count_lines(server.log_path, "PL-CORRUPTION")
    return cell


def rollback_cell(segments, workers=8):
    rows_per_txn = 1
    ops = 20
    phantom_base = 9000
    root, data_dir, sync_log, server = boot_cell(
        "rollback", "A", segments, "reuse", workers, rows_per_txn
    )
    cell = {"kind": "rollback", "segments_requested": segments, "workers": workers}
    port = server.port
    try:
        server.wait_ready()
        create_accounts(port)
        fill_wal(port, data_dir, segments)
        with connect(port) as conn:
            conn.cursor().execute(
                f"UPDATE {SCHEMA}.{TABLE} SET value = 0 WHERE id BETWEEN "
                f"{phantom_base + 1} AND {phantom_base + workers}"
            )

        def worker(wid):
            conn = connect(port, autocommit=False)
            cur = conn.cursor()
            own = row_for(wid, rows_per_txn)[0]
            phantom = phantom_base + wid + 1
            for i in range(ops):
                cur.execute(
                    f"UPDATE {SCHEMA}.{TABLE} SET value = value + 1 WHERE id = {own}"
                )
                if i % 2 == 0:
                    conn.commit()
                else:
                    cur.execute(
                        f"UPDATE {SCHEMA}.{TABLE} SET value = value + 1 WHERE id = {phantom}"
                    )
                    conn.rollback()
            conn.close()

        with ThreadPoolExecutor(max_workers=workers) as pool:
            list(pool.map(worker, range(workers)))
        with connect(port) as conn:
            cur = conn.cursor()
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id BETWEEN 1 AND {workers} "
                f"AND value = {ops // 2}"
            )
            cell["owned_correct"] = cur.fetchone()[0]
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id BETWEEN "
                f"{phantom_base + 1} AND {phantom_base + workers} AND value <> 0"
            )
            cell["phantom_nonzero"] = cur.fetchone()[0]
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id > {workers} "
                f"AND id < {phantom_base + 1} AND value <> 0"
            )
            cell["untouched_nonzero"] = cur.fetchone()[0]
        cell["ok"] = (
            cell["owned_correct"] == workers
            and cell["phantom_nonzero"] == 0
            and cell["untouched_nonzero"] == 0
        )
        print(f"  rollback seg{segments}: {cell}", flush=True)
    except Exception as exc:  # noqa: BLE001
        cell["ok"] = False
        cell["error"] = f"{type(exc).__name__}: {exc}"
        print(f"  rollback seg{segments} ERROR: {exc}", flush=True)
    finally:
        server.stop()
    return cell


def restart_cell(segments, workers=8):
    rows_per_txn = 1
    root, data_dir, sync_log, server = boot_cell(
        "restart", "A", segments, "reuse", workers, rows_per_txn
    )
    cell = {"kind": "restart", "segments_requested": segments, "workers": workers}
    port = server.port
    try:
        server.wait_ready()
        create_accounts(port)
        fill_wal(port, data_dir, segments)
        # committed workload (10 txns/worker) + rolled-back writes
        wall, _, errors = run_workload(port, workers, "reuse", 1, 10)
        cell["pre_restart_wall"] = wall
        cell["pre_restart_errors"] = errors
        cell["pre_restart_check"] = verify_owned(port, workers, 1, 10)
        # rolled-back row that must never become visible
        with connect(port, autocommit=False) as conn:
            cur = conn.cursor()
            cur.execute(
                f"UPDATE {SCHEMA}.{TABLE} SET value = value + 100 WHERE id = {rows_total_guard()}"
            )
            conn.rollback()

        server.stop()
        server2 = Server(data_dir, port, sync_log)
        t = time.perf_counter()
        server2.wait_ready(timeout=180)
        cell["restart_seconds"] = round(time.perf_counter() - t, 3)
        cell["post_restart_check"] = verify_owned(port, workers, 1, 10)
        with connect(port) as conn:
            cur = conn.cursor()
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} "
                f"WHERE id = {rows_total_guard()} AND value <> 0"
            )
            cell["rolled_back_row_nonzero"] = cur.fetchone()[0]
        # another workload after recovery
        wall2, _, errors2 = run_workload(port, 4, "reuse", 1, 20)
        cell["post_workload_wall"] = wall2
        cell["post_workload_errors"] = errors2
        with connect(port) as conn:
            cur = conn.cursor()
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id BETWEEN 1 AND 4 AND value = 30"
            )
            first = cur.fetchone()[0]
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id BETWEEN 5 AND 8 AND value = 10"
            )
            second = cur.fetchone()[0]
            cur.execute(
                f"SELECT count(*) FROM {SCHEMA}.{TABLE} WHERE id > 8 AND value <> 0"
            )
            other = cur.fetchone()[0]
        cell["post_workload_first4_30"] = first
        cell["post_workload_next4_10"] = second
        cell["post_workload_other_nonzero"] = other
        cell["ok"] = (
            cell["pre_restart_check"]["ok"]
            and cell["post_restart_check"]["ok"]
            and cell["rolled_back_row_nonzero"] == 0
            and first == 4
            and second == 4
            and other == 0
        )
        print(f"  restart seg{segments}: {cell}", flush=True)
    except Exception as exc:  # noqa: BLE001
        cell["ok"] = False
        cell["error"] = f"{type(exc).__name__}: {exc}"
        print(f"  restart seg{segments} ERROR: {exc}", flush=True)
    return cell


def rows_total_guard():
    return ROWS_TOTAL


# --------------------------------------------------------------------------
# summary
# --------------------------------------------------------------------------


def group_stats(reps):
    groups = [g for r in reps for g in r["groups"]]
    if not groups:
        return {}
    gt1 = sum(1 for g in groups if g > 1)
    return {
        "rounds": len(groups),
        "mean_group": statistics.mean(groups),
        "median_group": statistics.median(groups),
        "max_group": max(groups),
        "pct_groups_gt1": 100.0 * gt1 / len(groups),
        "distribution": {str(k): v for k, v in sorted(hist(groups).items())},
    }


def summarize(cell):
    reps = cell.get("reps", [])
    if not reps:
        return {}
    walls = [r["wall"] for r in reps]
    tps = [r["tx_per_sec"] for r in reps]
    groups = [g for r in reps for g in r["groups"]]
    flush_us = [u for r in reps for u in r["flush_us"]]
    durs = [u for r in reps for u in r["txn_commit_durability_wait_us"]]
    mutex = [u for r in reps for u in r["txn_commit_mutex_wait_us"]]
    pmutex = [u for r in reps for u in r["txn_commit_publish_mutex_wait_us"]]
    prepare = [u for r in reps for u in r["txn_commit_prepare_hold_us"]]
    publish = [u for r in reps for u in r["txn_commit_publish_hold_us"]]
    wal_append = [u for r in reps for u in r["commit_prepare_wal_append_us"]]
    storage = [u for r in reps for u in r["storage_apply_us"]]
    gate = [u for r in reps for u in r["write_gate_row_wait_us"]]
    tgate = [u for r in reps for u in r["write_gate_table_wait_us"]]
    lock = [u for r in reps for u in r["update_stmt_lock_us"]]
    return {
        "median_wall": statistics.median(walls),
        "min_wall": min(walls),
        "max_wall": max(walls),
        "median_tx_per_sec": statistics.median(tps),
        "transactions": reps[0]["transactions"],
        "p50_ms": statistics.median([r["p50_ms"] for r in reps]),
        "p95_ms": statistics.median([r["p95_ms"] for r in reps]),
        "p99_ms": statistics.median([r["p99_ms"] for r in reps]),
        "wal_syncs_total": sum(r["wal_syncs"] for r in reps),
        "rounds_total": sum(r["rounds"] for r in reps),
        "groups": group_stats(reps),
        "flush_p50_us": statistics.median(flush_us) if flush_us else None,
        "flush_p95_us": percentile(flush_us, 0.95),
        "durability_wait_p50_us": statistics.median(durs) if durs else None,
        "durability_wait_p95_us": percentile(durs, 0.95),
        "engine_mutex_wait_p50_us": statistics.median(mutex) if mutex else None,
        "engine_mutex_wait_p95_us": percentile(mutex, 0.95),
        "publish_mutex_wait_p50_us": statistics.median(pmutex) if pmutex else None,
        "publish_mutex_wait_p95_us": percentile(pmutex, 0.95),
        "prepare_hold_p50_us": statistics.median(prepare) if prepare else None,
        "prepare_hold_p95_us": percentile(prepare, 0.95),
        "publish_hold_p50_us": statistics.median(publish) if publish else None,
        "publish_hold_p95_us": percentile(publish, 0.95),
        "wal_append_p50_us": statistics.median(wal_append) if wal_append else None,
        "storage_apply_p50_us": statistics.median(storage) if storage else None,
        "storage_apply_p95_us": percentile(storage, 0.95),
        "row_gate_wait_p50_us": statistics.median(gate) if gate else None,
        "row_gate_wait_p95_us": percentile(gate, 0.95),
        "row_gate_wait_max_us": max(gate) if gate else None,
        "table_gate_wait_p95_us": percentile(tgate, 0.95),
        "update_lock_p50_us": statistics.median(lock) if lock else None,
        "update_lock_p95_us": percentile(lock, 0.95),
        "all_correct": all(r["check"].get("ok") for r in reps),
        "total_errors": sum(len(r["errors"]) for r in reps),
    }


def main():
    os.makedirs(DATA_ROOT, exist_ok=True)
    header = {
        "configs": CONFIGS,
        "w_set": W_SET,
        "reps": REPS,
        "a_ops": A_OPS,
        "b_ops": B_OPS,
        "b_rows": B_ROWS,
        "modes": MODES,
        "extra": EXTRA,
        "run_tag": RUN_TAG,
        "sync_shim": SHIM if os.path.exists(SHIM) else None,
    }
    print(json.dumps(header, indent=2), flush=True)
    results = {"header": header, "cells": []}
    # Resume support: a cell already measured under the same header is kept, so
    # an interrupted matrix can be continued without re-running finished cells.
    if os.path.exists(RESULTS):
        try:
            with open(RESULTS) as f:
                previous = json.load(f)
            if previous.get("header") == header:
                results["cells"] = previous.get("cells", [])
                print(f"resuming: {len(results['cells'])} cells already present", flush=True)
        except (ValueError, OSError):
            pass

    def already(kind, segments, mode, workers):
        return any(
            c.get("kind", "workload") == kind
            and c.get("segments_requested") == segments
            and c.get("mode") == mode
            and c.get("workers") == workers
            for c in results["cells"]
        )

    def save():
        with open(RESULTS, "w") as f:
            json.dump(results, f, indent=2)

    for spec in CONFIGS:
        workload, _, segments = spec.partition(":")
        segments = int(segments)
        for mode in MODES:
            for workers in W_SET:
                if already("workload", segments, mode, workers):
                    print(f"  skip workload{workload} seg{segments} {mode} W={workers}", flush=True)
                    continue
                cell = measure_workload_cell(workload, segments, mode, workers)
                cell["summary"] = summarize(cell)
                results["cells"].append(cell)
                save()
        if workload == "A":
            if "rollback" in EXTRA and not already("rollback", segments, None, 8):
                results["cells"].append(rollback_cell(segments))
                save()
            if "restart" in EXTRA and not already("restart", segments, None, 8):
                results["cells"].append(restart_cell(segments))
                save()
    save()
    print(f"\nresults written to {RESULTS}", flush=True)


if __name__ == "__main__":
    main()
