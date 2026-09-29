#!/usr/bin/env python3
"""Exact hot-row workload:

    UPDATE <schema>.counter SET value = value + 1 WHERE id = 1;

Two harness modes isolate the effect of connection handling on the measured
per-transaction cost:

    reconnect   one connection per transaction (mirrors prod_test.py's
                increment_counter: connect, execute, commit, close)
    reuse       one connection per worker, autocommit (the UPDATE itself)

Usage:
    DB_DSN=... python3 hot_row_update.py [reconnect|reuse ...]

Environment:
    WORKER_SET       worker counts       (default 1,2,4,8)
    OPS_PER_WORKER   transactions each   (default 10, matching the suite)
    REPEATS          repetitions         (default 1)
    TEST_SCHEMA      scratch schema
"""

import os
import statistics
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import psycopg

DSN = os.getenv("DB_DSN", "postgresql://plomid:plomid@127.0.0.1:5432/plomid")
SCHEMA = os.getenv("TEST_SCHEMA", "hotrow")
WORKER_SET = [int(w) for w in os.getenv("WORKER_SET", "1,2,4,8").split(",")]
OPS_PER_WORKER = int(os.getenv("OPS_PER_WORKER", "10"))
REPEATS = int(os.getenv("REPEATS", "1"))

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


def run(workers, mode):
    tx_latencies = []
    connect_latencies = []
    lock = threading.Lock()
    barrier = threading.Barrier(workers)

    def worker(_wid):
        if mode == "reuse":
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
            return

        barrier.wait()
        local = []
        local_connect = []
        for _ in range(OPS_PER_WORKER):
            start = time.perf_counter()
            conn = psycopg.connect(DSN, connect_timeout=5)
            local_connect.append(time.perf_counter() - start)
            cur = conn.cursor()
            start = time.perf_counter()
            cur.execute(SQL)
            conn.commit()
            local.append(time.perf_counter() - start)
            conn.close()
        with lock:
            tx_latencies.extend(local)
            connect_latencies.extend(local_connect)

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, w) for w in range(workers)]
        for future in futures:
            future.result()
    wall = time.perf_counter() - start

    ops = workers * OPS_PER_WORKER
    connect_note = ""
    if connect_latencies:
        connect_note = (
            f"  connect_avg={statistics.mean(connect_latencies) * 1000:6.2f}ms"
            f"  connect_total={sum(connect_latencies):6.3f}s"
        )
    print(
        f"  {mode:<9} W={workers:>2}  ops={ops:>4}  ops/s={ops / wall:8.1f}  "
        f"wall={wall:7.3f}s  avg={statistics.mean(tx_latencies) * 1000:7.2f}ms  "
        f"p50={percentile(tx_latencies, 0.50):7.2f}ms  "
        f"p95={percentile(tx_latencies, 0.95):7.2f}ms  "
        f"max={max(tx_latencies) * 1000:7.2f}ms{connect_note}",
        flush=True,
    )


MODES = {"reconnect": "reconnect", "reuse": "reuse"}


def main():
    names = sys.argv[1:] or [
        n for n in os.getenv("CASES", "").split(",") if n
    ] or list(MODES)
    reset()
    for repeat in range(REPEATS):
        if REPEATS > 1:
            print(f"--- repeat {repeat + 1}/{REPEATS}")
        for name in names:
            for workers in WORKER_SET:
                run(workers, name)
        print()


if __name__ == "__main__":
    main()
