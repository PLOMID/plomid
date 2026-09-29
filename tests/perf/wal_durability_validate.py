#!/usr/bin/env python3
"""Validate the WAL group-durability `flush_through` optimization end to end.

Measurement-only harness. It does not modify production code. For each
requested number of retained WAL segments it:

  1. boots the real `plomid-server` release binary against an explicitly
     isolated data directory (`--data`; never `PLOMID_DIR`),
  2. fills the WAL with genuinely retained segments and proves the requested
     number of `WAL-*.dat` files exists (`ls wal/` equivalent),
  3. runs the established hot-row workload
         UPDATE <schema>.counter SET value = value + 1 WHERE id = 1
     for W = 1/2/4/8 (8 workers x 10 transactions by default), in both
     connection-reuse and connection-reconnect modes,
  4. counts exact filesystem synchronization operations and the segment file
     each touched, via a measurement-only DYLD interposer
     (`target/bench-walval/libsyncshim.dylib`), and pairs them with the
     `plomid::perf` `group_flush` rounds, and
  5. verifies the final counter value, then (for the configured segment counts)
     restarts the server on the same data directory and re-verifies recovery,
     including a cold first commit that measures how many retained segments a
     flush touches when no per-segment durability state exists yet.

Environment overrides:
    SEGMENTS         retained-segment counts   (default 1,2,3,5,10)
    WORKER_SET       worker counts             (default 1,2,4,8)
    OPS_PER_WORKER   transactions per worker   (default 10)
    REPEATS          repetitions               (default 5)
    MODES            reuse,reconnect           (default both)
    SCRATCH          isolated data root        (default target/bench-walval/run)
    RESULTS          JSON output path          (default <SCRATCH>/results.json)
    RESTART_SEGMENTS configs to restart-test   (default 3,10)
"""

import json
import os
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
SCRATCH = os.getenv("SCRATCH", os.path.join(ROOT, "target", "bench-walval", "run"))
RESULTS = os.getenv("RESULTS", os.path.join(SCRATCH, "results.json"))

SEGMENT_BYTES = 8 * 1024 * 1024
FILLER_VALUE_BYTES = 15360  # largest row that fits the B+Tree page payload
FILLER_BATCH_ROWS = 64      # < one segment, so the file count grows by <= 1/batch
SCHEMA = "walval"
USER = "plomid"
PASSWORD = "plomid"

SEGMENT_COUNTS = [int(s) for s in os.getenv("SEGMENTS", "1,2,3,5,10").split(",")]
WORKER_SET = [int(w) for w in os.getenv("WORKER_SET", "1,2,4,8").split(",")]
OPS_PER_WORKER = int(os.getenv("OPS_PER_WORKER", "10"))
REPEATS = int(os.getenv("REPEATS", "5"))
MODES = [m for m in os.getenv("MODES", "reuse,reconnect").split(",") if m]
RESTART_SEGMENTS = [
    int(s) for s in os.getenv("RESTART_SEGMENTS", "3,10").split(",") if s
]

SQL = f"UPDATE {SCHEMA}.counter SET value = value + 1 WHERE id = 1"

_base = 56000 + (os.getpid() % 500) * 20
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
    def __init__(self, data_dir, port, sync_log, use_shim=True):
        self.data_dir = data_dir
        self.port = port
        self.sync_log = sync_log
        self.log_path = data_dir.rstrip("/") + ".server.log"
        os.makedirs(data_dir, exist_ok=True)
        env = dict(os.environ)
        if use_shim and os.path.exists(SHIM):
            env["DYLD_INSERT_LIBRARIES"] = SHIM
            env["PLOMID_SYNC_LOG"] = sync_log
        self._log = open(self.log_path, "a")
        self.proc = subprocess.Popen(
            [
                BIN,
                "--data", data_dir,
                "--host", "127.0.0.1",
                "--port", str(port),
                "--username", USER,
                "--password", PASSWORD,
                "--log-level", "info",
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

    def stop(self, timeout=120):
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
    return psycopg.connect(dsn(port), autocommit=autocommit, connect_timeout=10)


# --------------------------------------------------------------------------
# WAL retention setup and inspection
# --------------------------------------------------------------------------


def wal_listing(data_dir):
    d = os.path.join(data_dir, "wal")
    out = []
    for name in sorted(os.listdir(d)):
        p = os.path.join(d, name)
        if os.path.isfile(p) and name.startswith("WAL-") and name.endswith(".dat"):
            out.append((name, os.path.getsize(p)))
    return out


def fill_wal(port, data_dir, target):
    """Appends filler WAL until exactly `target` WAL segment files exist."""
    value = "x" * FILLER_VALUE_BYTES
    next_id = 1000
    with connect(port) as conn:
        conn.cursor().execute(
            f"CREATE TABLE IF NOT EXISTS {SCHEMA}.filler "
            "(id BIGINT PRIMARY KEY, v TEXT NOT NULL)"
        )
    batches = 0
    while len(wal_listing(data_dir)) < target:
        rows = []
        for _ in range(FILLER_BATCH_ROWS):
            next_id += 1
            rows.append(f"({next_id},'{value}')")
        sql = f"INSERT INTO {SCHEMA}.filler (id, v) VALUES {','.join(rows)}"
        with connect(port) as conn:
            conn.cursor().execute(sql)
        batches += 1
        count = len(wal_listing(data_dir))
        if count > target:
            raise RuntimeError(f"filler overshot target {target}: {wal_listing(data_dir)}")
    return batches


# --------------------------------------------------------------------------
# counter schema and workload
# --------------------------------------------------------------------------


def create_counter(port):
    with connect(port) as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"CREATE TABLE {SCHEMA}.counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)"
        )
        cur.execute(f"INSERT INTO {SCHEMA}.counter (id, value) VALUES (1, 0)")


def zero_counter(port):
    with connect(port) as conn:
        conn.cursor().execute(f"UPDATE {SCHEMA}.counter SET value = 0 WHERE id = 1")


def read_counter(port):
    with connect(port) as conn:
        cur = conn.cursor()
        cur.execute(f"SELECT value FROM {SCHEMA}.counter WHERE id = 1")
        return cur.fetchone()[0]


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * fraction))] * 1000.0


def run_workload(port, workers, mode):
    """Runs `workers` x OPS_PER_WORKER transactions; returns (wall, latencies)."""
    latencies = []
    lock = threading.Lock()
    barrier = threading.Barrier(workers)

    def worker(_wid):
        if mode == "reuse":
            conn = connect(port, autocommit=True)
            cur = conn.cursor()
            barrier.wait()
            local = []
            for _ in range(OPS_PER_WORKER):
                start = time.perf_counter()
                cur.execute(SQL)
                local.append(time.perf_counter() - start)
            conn.close()
            with lock:
                latencies.extend(local)
            return
        barrier.wait()
        local = []
        for _ in range(OPS_PER_WORKER):
            conn = connect(port, autocommit=False)
            cur = conn.cursor()
            start = time.perf_counter()
            cur.execute(SQL)
            conn.commit()
            local.append(time.perf_counter() - start)
            conn.close()
        with lock:
            latencies.extend(local)

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, w) for w in range(workers)]
        for future in futures:
            future.result()
    return time.perf_counter() - start, latencies


def run_one_commit(port):
    """One autocommit UPDATE (one transaction, one durability barrier)."""
    start = time.perf_counter()
    with connect(port) as conn:
        conn.cursor().execute(SQL)
    return time.perf_counter() - start


# --------------------------------------------------------------------------
# sync-log + perf-log parsing
# --------------------------------------------------------------------------


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
                ts = float(parts[1])
            except ValueError:
                continue
            entries.append((ts, parts[0], parts[2].strip()))
    return entries


def count_syncs(entries, t0, t1, wal_only=True):
    n = 0
    files = {}
    for ts, _kind, path in entries:
        if t0 <= ts <= t1:
            if wal_only and "/wal/" not in path:
                continue
            n += 1
            files[path] = files.get(path, 0) + 1
    return n, files


def read_group_flushes(log_path, start_offset):
    with open(log_path) as f:
        f.seek(start_offset)
        chunk = f.read()
    flushes = []
    for line in chunk.splitlines():
        if 'event="group_flush"' not in line:
            continue
        fields = {}
        for token in line.split():
            if "=" in token:
                key, _, val = token.partition("=")
                fields[key] = val
        try:
            flushes.append(
                {
                    "flush_us": int(fields.get("flush_us", "0")),
                    "flush_target": int(fields.get("flush_target", "0")),
                    "group": int(fields.get("group", "0")),
                }
            )
        except ValueError:
            continue
    return flushes


# --------------------------------------------------------------------------
# experiment driver
# --------------------------------------------------------------------------


def run_configuration(segment_count, results):
    tag = f"seg{segment_count}"
    root = os.path.join(SCRATCH, tag)
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)
    data_dir = os.path.join(root, "data")
    sync_log = os.path.join(root, "sync.log")
    port = next_port()

    server = Server(data_dir, port, sync_log)
    config = {"segments_requested": segment_count, "runs": {}, "restart": None}
    try:
        server.wait_ready()
        create_counter(port)
        if read_counter(port) != 0:
            raise RuntimeError("counter reset failed")
        t_fill = time.perf_counter()
        batches = fill_wal(port, data_dir, segment_count)
        config["fill_seconds"] = round(time.perf_counter() - t_fill, 3)
        config["fill_batches"] = batches
        listing = wal_listing(data_dir)
        config["verified_files"] = [name for name, _ in listing]
        config["verified_file_count"] = len(listing)
        config["verified_sizes"] = {name: size for name, size in listing}
        config["active_segment"] = listing[-1][0] if listing else None
        if len(listing) != segment_count:
            raise RuntimeError(f"expected {segment_count} segments, found {listing}")
        print(
            f"  {tag}: retained files={[n for n, _ in listing]} "
            f"active={config['active_segment']}",
            flush=True,
        )

        for mode in MODES:
            for rep in range(REPEATS):
                for workers in WORKER_SET:
                    zero_counter(port)
                    # Bracket by sync-log entry count: the reset's own WAL flush
                    # has already completed (and been logged) before this point.
                    time.sleep(0.05)
                    before = len(parse_sync_log(sync_log))
                    log_pos = server.log_size()
                    wall, latencies = run_workload(port, workers, mode)
                    time.sleep(0.25)
                    value = read_counter(port)
                    entries = parse_sync_log(sync_log)[before:]
                    wal_syncs, files = count_syncs(
                        entries, float("-inf"), float("inf"), wal_only=True
                    )
                    all_syncs, _ = count_syncs(
                        entries, float("-inf"), float("inf"), wal_only=False
                    )
                    flushes = read_group_flushes(server.log_path, log_pos)
                    ops = workers * OPS_PER_WORKER
                    key = f"{mode}-W{workers}"
                    record = config["runs"].setdefault(
                        key, {"workers": workers, "mode": mode, "reps": []}
                    )
                    record["reps"].append(
                        {
                            "wall": wall,
                            "ops_per_sec": ops / wall,
                            "ops": ops,
                            "p50_ms": percentile(latencies, 0.50),
                            "p95_ms": percentile(latencies, 0.95),
                            "max_ms": max(latencies) * 1000.0,
                            "wal_syncs": wal_syncs,
                            "all_fsyncs": all_syncs,
                            "wal_files_synced": files,
                            "rounds": len(flushes),
                            "syncs_per_round": (
                                round(wal_syncs / len(flushes), 3) if flushes else None
                            ),
                            "flush_us": [f["flush_us"] for f in flushes],
                            "groups": [f["group"] for f in flushes],
                            "counter": value,
                            "counter_ok": value == ops,
                        }
                    )
                    print(
                        f"    {mode:<9} rep={rep + 1} W={workers:>2} "
                        f"wall={wall:7.3f}s ops/s={ops / wall:7.1f} "
                        f"p50={percentile(latencies, 0.5):6.2f}ms "
                        f"p95={percentile(latencies, 0.95):6.2f}ms "
                        f"wal_syncs={wal_syncs:>3} rounds={len(flushes):>3} "
                        f"value={value}",
                        flush=True,
                    )

        listing_after = wal_listing(data_dir)
        config["files_after_workload"] = [name for name, _ in listing_after]

        # Deterministic final state for the restart check.
        zero_counter(port)
        _, _ = run_workload(port, 2, "reuse")
        config["pre_restart_counter"] = read_counter(port)
        config["pre_restart_expected"] = 2 * OPS_PER_WORKER

        if segment_count in RESTART_SEGMENTS:
            config["restart"] = restart_validation(
                server, data_dir, sync_log, port, segment_count
            )
    finally:
        server.stop()
    results.append(config)


def restart_validation(server, data_dir, sync_log, port, segment_count):
    """Stop, restart on the same data dir, verify recovery, probe flush behavior."""
    server.stop()
    out = {}
    server2 = Server(data_dir, port, sync_log)
    try:
        t_restart = time.perf_counter()
        server2.wait_ready(timeout=180)
        out["restart_seconds"] = round(time.perf_counter() - t_restart, 3)
        out["counter_after_restart"] = read_counter(port)
        out["recovered_ok"] = out["counter_after_restart"] == 2 * OPS_PER_WORKER
        listing = wal_listing(data_dir)
        out["files_after_restart"] = [name for name, _ in listing]
        out["file_count_after_restart"] = len(listing)

        # Cold first commit: GroupDurability state is empty after restart, so
        # this flush touches every retained segment whose range covers target.
        log_pos = server2.log_size()
        time.sleep(0.05)
        before = len(parse_sync_log(sync_log))
        cold_wall = run_one_commit(port)
        time.sleep(0.25)
        entries = parse_sync_log(sync_log)[before:]
        cold_syncs, cold_files = count_syncs(
            entries, float("-inf"), float("inf"), wal_only=True
        )
        out["cold_commit_wall"] = cold_wall
        out["cold_commit_wal_syncs"] = cold_syncs
        out["cold_commit_files_synced"] = cold_files
        out["cold_commit_rounds"] = len(read_group_flushes(server2.log_path, log_pos))

        # Warm second commit: per-segment durability state now covers the
        # sealed segments, so only the active segment is re-synchronized.
        log_pos = server2.log_size()
        before = len(parse_sync_log(sync_log))
        warm_wall = run_one_commit(port)
        time.sleep(0.25)
        entries = parse_sync_log(sync_log)[before:]
        warm_syncs, warm_files = count_syncs(
            entries, float("-inf"), float("inf"), wal_only=True
        )
        out["warm_commit_wall"] = warm_wall
        out["warm_commit_wal_syncs"] = warm_syncs
        out["warm_commit_files_synced"] = warm_files
        out["warm_commit_rounds"] = len(read_group_flushes(server2.log_path, log_pos))

        # A small normal workload afterwards must commit successfully.
        before_value = read_counter(port)
        time.sleep(0.05)
        before = len(parse_sync_log(sync_log))
        wall, _ = run_workload(port, 2, "reuse")
        time.sleep(0.25)
        value = read_counter(port)
        entries = parse_sync_log(sync_log)[before:]
        wal_syncs, files = count_syncs(
            entries, float("-inf"), float("inf"), wal_only=True
        )
        out["post_workload_wall"] = wall
        out["post_workload_delta"] = value - before_value
        out["post_workload_expected_delta"] = 2 * OPS_PER_WORKER
        out["post_workload_ok"] = value - before_value == 2 * OPS_PER_WORKER
        out["post_workload_wal_syncs"] = wal_syncs
        out["post_workload_files_synced"] = files
        print(
            f"    restart(seg{segment_count}): recovered={out['recovered_ok']} "
            f"counter={out['counter_after_restart']} "
            f"cold_syncs={out['cold_commit_wal_syncs']} "
            f"warm_syncs={out['warm_commit_wal_syncs']} "
            f"post_delta={out['post_workload_delta']}",
            flush=True,
        )
    finally:
        server2.stop()
    return out


def summarize(config):
    rows = []
    for key, rec in config["runs"].items():
        reps = rec["reps"]
        walls = [r["wall"] for r in reps]
        ops = [r["ops_per_sec"] for r in reps]
        flats = [u for r in reps for u in r["flush_us"]]
        spr = [r["syncs_per_round"] for r in reps if r["syncs_per_round"] is not None]
        rows.append(
            {
                "workers": rec["workers"],
                "mode": rec["mode"],
                "median_wall": statistics.median(walls),
                "min_wall": min(walls),
                "max_wall": max(walls),
                "median_ops_per_sec": statistics.median(ops),
                "p50_ms": statistics.median([r["p50_ms"] for r in reps]),
                "p95_ms": statistics.median([r["p95_ms"] for r in reps]),
                "wal_syncs_total": sum(r["wal_syncs"] for r in reps),
                "rounds_total": sum(r["rounds"] for r in reps),
                "syncs_per_round_median": statistics.median(spr) if spr else None,
                "flush_p50_us": statistics.median(flats) if flats else None,
                "flush_p95_us": percentile(flats, 0.95) if flats else None,
                "flush_max_us": max(flats) if flats else None,
                "counter_ok": all(r["counter_ok"] for r in reps),
            }
        )
    return rows


def main():
    os.makedirs(SCRATCH, exist_ok=True)
    if not os.path.exists(SHIM):
        print(f"WARNING: sync interposer {SHIM} not found", file=sys.stderr)
    print(
        json.dumps(
            {
                "binary": BIN,
                "shim": SHIM if os.path.exists(SHIM) else None,
                "segments": SEGMENT_COUNTS,
                "worker_set": WORKER_SET,
                "ops_per_worker": OPS_PER_WORKER,
                "repeats": REPEATS,
                "modes": MODES,
            },
            indent=2,
        ),
        flush=True,
    )
    results = []
    for segment_count in SEGMENT_COUNTS:
        print(f"=== {segment_count} retained WAL segment(s) ===", flush=True)
        run_configuration(segment_count, results)
    for config in results:
        config["summary"] = summarize(config)
    with open(RESULTS, "w") as f:
        json.dump(results, f, indent=2)
    print(f"\nresults written to {RESULTS}", flush=True)


if __name__ == "__main__":
    main()
