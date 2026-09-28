#!/usr/bin/env python3
"""Concurrency scaling: independent writes at W = 1/2/4/8/16/32.

Independent transactions (different rows, different keys) must be able to
overlap for throughput to scale. Same-row UPDATE is measured for contrast:
contention there is expected and does not justify global serialization.

Usage: DB_DSN=... python3 concurrency_bench.py [insert|update|samerow ...]
"""

import os
import statistics
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

import psycopg

DSN = os.getenv("DB_DSN", "postgresql://plomid:plomid@127.0.0.1:5432/plomid")
SCHEMA = os.getenv("TEST_SCHEMA", "perf_conc")
WORKER_SET = [int(w) for w in os.getenv("WORKER_SET", "1,2,4,8,16,32").split(",")]
OPS_PER_WORKER = int(os.getenv("OPS_PER_WORKER", "64"))


def connect():
    return psycopg.connect(DSN, autocommit=True, connect_timeout=5)


def reset():
    with connect() as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"CREATE TABLE {SCHEMA}.ins (id BIGINT PRIMARY KEY, name TEXT NOT NULL, v INTEGER)"
        )
        cur.execute(
            f"CREATE TABLE {SCHEMA}.upd (id BIGINT PRIMARY KEY, v INTEGER NOT NULL)"
        )
        cur.execute(
            f"CREATE TABLE {SCHEMA}.same (id BIGINT PRIMARY KEY, v INTEGER NOT NULL)"
        )
        cur.execute(f"INSERT INTO {SCHEMA}.same (id, v) VALUES (1, 0)")


def run_workload(workers, worker_fn, ops_per_worker=OPS_PER_WORKER):
    """Runs ops_per_worker transactions per worker; returns (ops, wall, latencies)."""
    latencies = []
    lock = threading.Lock()
    barrier = threading.Barrier(workers)

    def worker(wid):
        conn = connect()
        cur = conn.cursor()
        barrier.wait()
        local = []
        for i in range(ops_per_worker):
            start = time.perf_counter()
            worker_fn(cur, wid, i)
            local.append(time.perf_counter() - start)
        conn.close()
        with lock:
            latencies.extend(local)

    start = time.perf_counter()
    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(worker, w) for w in range(workers)]
        for f in futures:
            f.result()
    wall = time.perf_counter() - start
    ops = workers * ops_per_worker
    return ops, wall, latencies


def bench_insert():
    print("independent INSERT (distinct keys):")
    print(f"  {'W':>3}  {'ops/s':>8}  {'p50 ms':>8}  {'p95 ms':>8}")
    base = [0]

    def run():
        for workers in WORKER_SET:
            counter = [workers * 10_000_000 + base[0]]
            base[0] += 1

            def worker_fn(cur, wid, i, counter=counter, workers=workers):
                key = counter[0] + wid * 100_000 + i
                cur.execute(
                    f"INSERT INTO {SCHEMA}.ins (id, name, v) VALUES (%s, %s, %s)",
                    (key, f"w{wid}", i % 100),
                )

            ops, wall, lat = run_workload(workers, worker_fn)
            lat.sort()
            p50 = lat[len(lat) // 2] * 1000
            p95 = lat[int(len(lat) * 0.95)] * 1000
            print(f"  {workers:>3}  {ops / wall:8.0f}  {p50:8.2f}  {p95:8.2f}")

    run()


def bench_update():
    print("independent UPDATE (distinct rows):")
    print(f"  {'W':>3}  {'ops/s':>8}  {'p50 ms':>8}  {'p95 ms':>8}")
    with connect() as conn:
        cur = conn.cursor()
        cur.executemany(
            f"INSERT INTO {SCHEMA}.upd (id, v) VALUES (%s, %s)",
            [(i, 0) for i in range(1, 4097)],
        )

    for workers in WORKER_SET:
        counter = [0]

        def worker_fn(cur, wid, i, counter=counter, workers=workers):
            counter[0] += 1
            key = (counter[0] - 1) % 4096 + 1
            cur.execute(f"UPDATE {SCHEMA}.upd SET v = v + 1 WHERE id = %s", (key,))

        ops, wall, lat = run_workload(workers, worker_fn, ops_per_worker=32)
        lat.sort()
        p50 = lat[len(lat) // 2] * 1000
        p95 = lat[int(len(lat) * 0.95)] * 1000
        print(f"  {workers:>3}  {ops / wall:8.0f}  {p50:8.2f}  {p95:8.2f}")


def bench_samerow():
    print("same-row UPDATE (intentional contention):")
    print(f"  {'W':>3}  {'ops/s':>8}  {'p50 ms':>8}  {'p95 ms':>8}")
    for workers in WORKER_SET:
        def worker_fn(cur, wid, i):
            cur.execute(f"UPDATE {SCHEMA}.same SET v = v + 1 WHERE id = 1")

        ops, wall, lat = run_workload(workers, worker_fn, ops_per_worker=32)
        lat.sort()
        p50 = lat[len(lat) // 2] * 1000
        p95 = lat[int(len(lat) * 0.95)] * 1000
        print(f"  {workers:>3}  {ops / wall:8.0f}  {p50:8.2f}  {p95:8.2f}")


BENCHES = {
    "insert": bench_insert,
    "update": bench_update,
    "samerow": bench_samerow,
}


def main():
    names = sys.argv[1:] or list(BENCHES)
    reset()
    for name in names:
        BENCHES[name]()


if __name__ == "__main__":
    main()
