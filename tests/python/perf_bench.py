#!/usr/bin/env python3
"""Focused performance benchmark for PLOMID / PostgreSQL.

Measures the workloads that the production compatibility suite reports as the
largest gaps: point DML, executemany, bulk insert, commit, concurrency and
repeated execution.  Prints median wall-clock per workload so runs can be
compared before/after.
"""

import os
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from decimal import Decimal

import psycopg

DSN = os.getenv("DB_DSN", "postgresql://plomid:plomid@127.0.0.1:5433/plomid")
SCHEMA = os.getenv("TEST_SCHEMA", "perf_bench")
REPS = int(os.getenv("REPS", "5"))
WORKERS = int(os.getenv("WORKERS", "8"))
STRESS_ROWS = int(os.getenv("STRESS_ROWS", "200"))


def connect(autocommit=False):
    return psycopg.connect(DSN, autocommit=autocommit, connect_timeout=5)


def med(fn, reps=REPS):
    samples = []
    for _ in range(reps):
        start = time.perf_counter()
        fn()
        samples.append(time.perf_counter() - start)
    return statistics.median(samples), min(samples)


def reset():
    with connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"""
            CREATE TABLE {SCHEMA}.users (
                id BIGINT PRIMARY KEY,
                name TEXT NOT NULL,
                age INTEGER,
                salary NUMERIC(14,2),
                active BOOLEAN,
                email TEXT UNIQUE,
                metadata JSONB
            )
            """
        )
        cur.execute(f"CREATE INDEX idx_users_name ON {SCHEMA}.users(name)")


def bench_select():
    def run():
        with connect(autocommit=True) as conn:
            cur = conn.cursor()
            cur.execute("SELECT 10 + 20 * 2")
            cur.fetchone()

    return med(run)


def bench_insert():
    counter = [0]

    def run():
        counter[0] += 1
        i = counter[0]
        with connect() as conn:
            cur = conn.cursor()
            cur.execute(
                f"INSERT INTO {SCHEMA}.users (id, name, age) VALUES (%s, %s, %s)",
                (i, f"single-{i}", 20 + i % 40),
            )
            conn.commit()

    return med(run)


def bench_update():
    def run():
        with connect() as conn:
            cur = conn.cursor()
            cur.execute(f"UPDATE {SCHEMA}.users SET age = age + 1 WHERE id = 1")
            conn.commit()

    return med(run)


def bench_delete():
    counter = [0]

    def run():
        counter[0] += 1
        i = 900000 + counter[0]
        with connect() as conn:
            cur = conn.cursor()
            cur.execute(
                f"INSERT INTO {SCHEMA}.users (id, name) VALUES (%s, %s)", (i, f"del-{i}")
            )
            conn.commit()

    return med(run)


def bench_executemany(rows=100):
    counter = [0]

    def run():
        base = 1000000 + counter[0] * 10000
        counter[0] += 1
        params = [(base + i, f"em-{base + i}", 20 + i % 40) for i in range(rows)]
        with connect() as conn:
            cur = conn.cursor()
            cur.executemany(
                f"INSERT INTO {SCHEMA}.users (id, name, age) VALUES (%s, %s, %s)",
                params,
            )
            conn.commit()

    return med(run, reps=3)


def bench_large_insert(rows=STRESS_ROWS):
    counter = [0]

    def run():
        base = 5000000 + counter[0] * 1000000
        counter[0] += 1
        params = [(base + i, f"stress-{base + i}", i % 100) for i in range(rows)]
        with connect() as conn:
            cur = conn.cursor()
            cur.executemany(
                f"INSERT INTO {SCHEMA}.users (id, name, age) VALUES (%s, %s, %s)",
                params,
            )
            conn.commit()

    return med(run, reps=3)


def bench_large_select():
    def run():
        with connect(autocommit=True) as conn:
            cur = conn.cursor()
            cur.execute(f"SELECT id, name, age FROM {SCHEMA}.users")
            cur.fetchall()

    return med(run, reps=3)


def bench_repeated():
    def run():
        with connect(autocommit=True) as conn:
            cur = conn.cursor()
            for _ in range(1000):
                cur.execute("SELECT 1")
                cur.fetchone()

    return med(run, reps=3)


def counter_setup():
    with connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute(
            f"CREATE TABLE IF NOT EXISTS {SCHEMA}.counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)"
        )
        cur.execute(f"DELETE FROM {SCHEMA}.counter")
        cur.execute(f"INSERT INTO {SCHEMA}.counter(id, value) VALUES (1, 0)")


def bench_concurrent_update():
    counter_setup()

    def worker(_):
        with connect() as conn:
            cur = conn.cursor()
            cur.execute(f"UPDATE {SCHEMA}.counter SET value = value + 1 WHERE id = 1")
            conn.commit()

    ops = WORKERS * 10

    def run():
        with ThreadPoolExecutor(max_workers=WORKERS) as pool:
            futures = [pool.submit(worker, i) for i in range(ops)]
            for f in as_completed(futures):
                f.result()

    return med(run, reps=3)


def bench_concurrent_insert():
    counter = [0]

    def run():
        base = counter[0] * 1000
        counter[0] += 1

        def worker_unique(wid):
            with connect() as conn:
                cur = conn.cursor()
                cur.execute(
                    f"INSERT INTO {SCHEMA}.users (id, name, age) VALUES (%s, %s, %s)",
                    (base + 3000000 + wid, f"worker-{base + wid}", 20),
                )
                conn.commit()

        with ThreadPoolExecutor(max_workers=WORKERS) as pool:
            futures = [pool.submit(worker_unique, i) for i in range(WORKERS)]
            for f in as_completed(futures):
                f.result()

    return med(run, reps=3)


def main():
    reset()
    results = {}
    plan = [
        ("SELECT", bench_select),
        ("INSERT", bench_insert),
        ("UPDATE", bench_update),
        ("DELETE", bench_delete),
        ("executemany100", bench_executemany),
        ("largeINSERT200", bench_large_insert),
        ("largeSELECT", bench_large_select),
        ("repeated1000", bench_repeated),
        ("concurrentINSERT", bench_concurrent_insert),
        ("concurrentUPDATE", bench_concurrent_update),
    ]
    only = sys.argv[1:] or None
    for name, fn in plan:
        if only and name not in only:
            continue
        m, mn = fn()
        results[name] = m
        print(f"{name:20s} median={m:8.4f}s  min={mn:8.4f}s", flush=True)
    total = sum(results.values())
    print(f"{'TOTAL':20s} {total:.4f}s")


if __name__ == "__main__":
    main()
