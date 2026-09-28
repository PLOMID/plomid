#!/usr/bin/env python3
"""Focused concurrent-UPDATE benchmark.

Three cases separate legitimate contention from unnecessary serialization:

    rows    independent rows (each worker owns a disjoint key range)
    same    one shared row (real row-level contention; must serialize)
    tables  independent tables (one table per worker)

Usage:
    DB_DSN=... python3 concurrent_update.py [rows|same|tables ...]

Environment:
    WORKER_SET       comma list of worker counts   (default 1,2,4,8,16)
    OPS_PER_WORKER   transactions per worker       (default 32)
    ROWS             rows per table for `rows`     (default 8192)
"""

import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import psycopg

DSN = os.getenv("DB_DSN", "postgresql://plomid:plomid@127.0.0.1:5432/plomid")
SCHEMA = os.getenv("TEST_SCHEMA", "perf_update")
WORKER_SET = [int(w) for w in os.getenv("WORKER_SET", "1,2,4,8,16").split(",")]
OPS_PER_WORKER = int(os.getenv("OPS_PER_WORKER", "32"))
REPEATS = int(os.getenv("REPEATS", "1"))
ROWS = int(os.getenv("ROWS", "8192"))
MAX_WORKERS = max(WORKER_SET)


def connect():
    return psycopg.connect(DSN, autocommit=True, connect_timeout=5)


def reset():
    with connect() as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"CREATE TABLE {SCHEMA}.indep (id BIGINT PRIMARY KEY, v BIGINT NOT NULL)"
        )
        cur.execute(
            f"CREATE TABLE {SCHEMA}.same (id BIGINT PRIMARY KEY, v BIGINT NOT NULL)"
        )
        cur.execute(f"INSERT INTO {SCHEMA}.same (id, v) VALUES (1, 0)")
        cur.executemany(
            f"INSERT INTO {SCHEMA}.indep (id, v) VALUES (%s, 0)",
            [(i,) for i in range(1, ROWS + 1)],
        )
        for w in range(MAX_WORKERS):
            cur.execute(
                f"CREATE TABLE {SCHEMA}.t{w} (id BIGINT PRIMARY KEY, v BIGINT NOT NULL)"
            )
            cur.execute(f"INSERT INTO {SCHEMA}.t{w} (id, v) VALUES (1, 0)")


def run_case(label, workers, work):
    """Runs `work(cur, wid, i)` on `workers` connections; returns metrics."""
    latencies = []
    lock = threading.Lock()
    barrier = threading.Barrier(workers)

    def worker(wid):
        conn = connect()
        try:
            cur = conn.cursor()
            barrier.wait()
            local = []
            for i in range(OPS_PER_WORKER):
                start = time.perf_counter()
                work(cur, wid, i)
                local.append(time.perf_counter() - start)
        finally:
            conn.close()
        with lock:
            latencies.extend(local)

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, w) for w in range(workers)]
        for future in futures:
            future.result()
    wall = time.perf_counter() - start

    ops = workers * OPS_PER_WORKER
    latencies.sort()
    p50 = latencies[len(latencies) // 2] * 1000
    p95 = latencies[min(len(latencies) - 1, int(len(latencies) * 0.95))] * 1000
    avg = sum(latencies) / len(latencies) * 1000
    print(
        f"  {label:<8} W={workers:>2}  ops={ops:>5}  "
        f"ops/s={ops / wall:9.1f}  wall={wall:7.3f}s  "
        f"avg={avg:7.2f}ms  p50={p50:7.2f}ms  p95={p95:7.2f}ms",
        flush=True,
    )
    return ops / wall


def bench_rows():
    print("independent rows (disjoint key range per worker):")
    per_worker = max(1, ROWS // MAX_WORKERS)

    def work(cur, wid, i):
        key = wid * per_worker + (i % per_worker) + 1
        cur.execute(f"UPDATE {SCHEMA}.indep SET v = v + 1 WHERE id = %s", (key,))

    for workers in WORKER_SET:
        run_case("rows", workers, work)


def bench_same():
    print("same row (intentional row-level contention):")

    def work(cur, wid, i):
        cur.execute(f"UPDATE {SCHEMA}.same SET v = v + 1 WHERE id = 1")

    for workers in WORKER_SET:
        run_case("same", workers, work)


def bench_tables():
    print("independent tables (one table per worker):")

    def work(cur, wid, i):
        cur.execute(f"UPDATE {SCHEMA}.t{wid % MAX_WORKERS} SET v = v + 1 WHERE id = 1")

    for workers in WORKER_SET:
        run_case("tables", workers, work)


CASES = {"rows": bench_rows, "same": bench_same, "tables": bench_tables}


def main():
    names = sys.argv[1:] or [
        n for n in os.getenv("CASES", "").split(",") if n
    ] or list(CASES)
    reset()
    for repeat in range(REPEATS):
        if REPEATS > 1:
            print(f"--- repeat {repeat + 1}/{REPEATS}")
        for name in names:
            CASES[name]()
        print()


if __name__ == "__main__":
    main()
