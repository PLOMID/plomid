#!/usr/bin/env python3
"""Benchmark WAL durability flush optimization.

Measures the effect of per-segment durability tracking on commit latency
with varying numbers of retained WAL segments.

Uses the hot-row workload (UPDATE single row) to stress the commit path.

Environment:
    DB_DSN           connection string (default postgresql://plomid:plomid@127.0.0.1:5432/plomid)
    WORKER_SET       worker counts (default 1,2,4,8)
    OPS_PER_WORKER   transactions per worker (default 50)
    REPEATS          repetitions (default 1)
    TEST_SCHEMA      scratch schema (default walbench)
    SEGMENTS         number of retained WAL segments to test (default 1,2,3,5,10)
"""

import os
import statistics
import subprocess
import sys
import time

import psycopg

DSN = os.getenv("DB_DSN", "postgresql://plomid:plomid@127.0.0.1:5432/plomid")
SCHEMA = os.getenv("TEST_SCHEMA", "walbench")
WORKER_SET = [int(w) for w in os.getenv("WORKER_SET", "1,2,4,8").split(",")]
OPS_PER_WORKER = int(os.getenv("OPS_PER_WORKER", "50"))
REPEATS = int(os.getenv("REPEATS", "1"))
SEGMENT_COUNTS = [int(s) for s in os.getenv("SEGMENTS", "1,2,3,5,10").split(",")]

SQL = f"UPDATE {SCHEMA}.counter SET value = value + 1 WHERE id = 1"


def reset():
    with psycopg.connect(DSN, autocommit=True, connect_timeout=5) as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"CREATE TABLE {SCHEMA}.counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)"
        )
        cur.execute(f"INSERT INTO {SCHEMA}.counter(id, value) VALUES (1, 0)")


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * fraction))] * 1000


def get_wal_segment_count():
    """Read the number of WAL segment files from the server's WAL directory."""
    import psycopg
    # Use a superuser function or pg_read_file if available
    # For plomid, we can query the WAL directory via a custom function
    try:
        with psycopg.connect(DSN, autocommit=True, connect_timeout=5) as conn:
            cur = conn.cursor()
            cur.execute("SELECT plomid_wal_segment_count()")
            return cur.fetchone()[0]
    except Exception:
        # Fallback: return None if function doesn't exist
        return None


def run(workers, segment_count):
    """Run the hot-row benchmark and return metrics."""
    tx_latencies = []
    lock = threading.Lock()
    barrier = threading.Barrier(workers)

    def worker(_wid):
        conn = psycopg.connect(DSN, autocommit=True, connect_timeout=5)
        cur = conn.cursor()
        barrier.wait()
        local = []
        for _ in range(OPS_PER_WORKER):
            start = time.perf_counter()
            cur.execute(SQL)
            local.append(time.perf_counter() - start)
        conn.close()
        with lock:
            tx_latencies.extend(local)

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, w) for w in range(workers)]
        for future in futures:
            future.result()
    wall = time.perf_counter() - start

    ops = workers * OPS_PER_WORKER
    return {
        "workers": workers,
        "segment_count": segment_count,
        "ops": ops,
        "wall": wall,
        "ops_per_sec": ops / wall,
        "avg_ms": statistics.mean(tx_latencies) * 1000,
        "p50_ms": percentile(tx_latencies, 0.50),
        "p95_ms": percentile(tx_latencies, 0.95),
        "max_ms": max(tx_latencies) * 1000,
    }


def main():
    import threading
    from concurrent.futures import ThreadPoolExecutor

    names = sys.argv[1:] or ["all"]
    reset()

    print(f"=== WAL Durability Flush Benchmark ===")
    print(f"WORKER_SET={WORKER_SET}")
    print(f"OPS_PER_WORKER={OPS_PER_WORKER}")
    print(f"SEGMENT_COUNTS={SEGMENT_COUNTS}")
    print()

    for repeat in range(REPEATS):
        if REPEATS > 1:
            print(f"--- repeat {repeat + 1}/{REPEATS} ---")
        for seg_count in SEGMENT_COUNTS:
            print(f"--- {seg_count} retained WAL segments ---")
            for workers in WORKER_SET:
                metrics = run(workers, seg_count)
                print(
                    f"  W={metrics['workers']:>2}  ops={metrics['ops']:>4}  "
                    f"ops/s={metrics['ops_per_sec']:8.1f}  wall={metrics['wall']:7.3f}s  "
                    f"avg={metrics['avg_ms']:7.2f}ms  p50={metrics['p50_ms']:7.2f}ms  "
                    f"p95={metrics['p95_ms']:7.2f}ms  max={metrics['max_ms']:7.2f}ms"
                )
            print()
        print()


if __name__ == "__main__":
    main()
