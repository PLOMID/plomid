#!/usr/bin/env python3
"""Minimal PLOMID latency probe (independent of the big benchmark).

Measures, separately:
  * connection establishment
  * first query vs repeated queries
  * 1-row INSERT / SELECT / UPDATE / DELETE, p50/p95/min/max
  * simple-query (literal SQL) vs extended-query (parameterized) protocol
  * autocommit-per-statement vs explicit per-statement COMMIT
  * concurrency scaling (1/2/4/8 connections)

Usage:
    python3 tests/perf/latency_probe.py --dsn postgresql://plomid@127.0.0.1:16000/plomid
"""

import argparse
import statistics
import sys
import threading
import time

import psycopg2


def pct(values, p):
    if not values:
        return 0.0
    ordered = sorted(values)
    pos = (len(ordered) - 1) * (p / 100.0)
    lo = int(pos)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (pos - lo)


def report(name, latencies):
    ms = [v * 1000 for v in latencies]
    print(
        f"  {name:<34} n={len(ms):<5} "
        f"p50={pct(ms,50):8.3f}ms p95={pct(ms,95):8.3f}ms "
        f"min={min(ms):8.3f}ms max={max(ms):8.3f}ms "
        f"mean={statistics.mean(ms):8.3f}ms"
    )


def connect(dsn):
    start = time.perf_counter()
    conn = psycopg2.connect(dsn, connect_timeout=30)
    return conn, time.perf_counter() - start


def setup(dsn):
    conn, _ = connect(dsn)
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS probe")
        cur.execute(
            """
            CREATE TABLE probe (
                id          BIGINT PRIMARY KEY,
                uk          VARCHAR(64) UNIQUE,
                val         INTEGER NOT NULL,
                payload     TEXT
            )
            """
        )
    conn.close()


def measure_batch(dsn, label, sql, params_fn, count, autocommit, commit_each=False):
    conn, _ = connect(dsn)
    conn.autocommit = autocommit
    latencies = []
    with conn.cursor() as cur:
        for i in range(count):
            params = params_fn(i)
            start = time.perf_counter()
            if params is None:
                cur.execute(sql)
            else:
                cur.execute(sql, params)
            if cur.description is not None:
                cur.fetchall()
            if commit_each:
                conn.commit()
            latencies.append(time.perf_counter() - start)
    conn.close()
    report(label, latencies)
    return latencies


def concurrency_scaling(dsn, concurrency, ops_per_worker, mode):
    """Return (wall_seconds, per_op_latencies)."""
    latencies = []
    lock = threading.Lock()

    def worker(worker_id):
        conn, _ = connect(dsn)
        conn.autocommit = True
        local = []
        with conn.cursor() as cur:
            for i in range(ops_per_worker):
                row_id = 1_000_000 + worker_id * ops_per_worker + i
                start = time.perf_counter()
                if mode == "insert":
                    cur.execute(
                        "INSERT INTO probe (id, uk, val, payload) VALUES (%s, %s, %s, %s)",
                        (row_id, f"uk-{row_id}", i, "x" * 40),
                    )
                elif mode == "update":
                    cur.execute(
                        "UPDATE probe SET val = val + 1 WHERE id = %s", (row_id,)
                    )
                else:
                    cur.execute("SELECT val FROM probe WHERE id = %s", (row_id,))
                if cur.description is not None:
                    cur.fetchall()
                local.append(time.perf_counter() - start)
        conn.close()
        with lock:
            latencies.extend(local)

    threads = [threading.Thread(target=worker, args=(w,)) for w in range(concurrency)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - start
    return wall, latencies


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--dsn", default="postgresql://plomid@127.0.0.1:16000/plomid"
    )
    parser.add_argument("--count", type=int, default=200)
    parser.add_argument("--concurrency", type=int, default=8)
    parser.add_argument("--ops", type=int, default=50)
    parser.add_argument("--skip-setup", action="store_true")
    args = parser.parse_args()

    if not args.skip_setup:
        setup(args.dsn)
        print("setup: probe table created")

    print("\n== connection ==")
    conns = []
    for _ in range(10):
        _c, elapsed = connect(args.dsn)
        conns.append(elapsed)
        _c.close()
    report("connect", conns)

    print("\n== cold/warm first query ==")
    conn, elapsed = connect(args.dsn)
    conn.autocommit = True
    start = time.perf_counter()
    with conn.cursor() as cur:
        cur.execute("SELECT 1")
        cur.fetchall()
    print(f"  first_query_ms={ (time.perf_counter()-start)*1000:.3f}")
    first_batch = []
    with conn.cursor() as cur:
        for _ in range(20):
            start = time.perf_counter()
            cur.execute("SELECT 1")
            cur.fetchall()
            first_batch.append(time.perf_counter() - start)
    report("select_1 (next 20)", first_batch)
    conn.close()

    print("\n== single-row operations (autocommit per statement) ==")
    n = args.count
    measure_batch(
        args.dsn,
        "insert_simple_protocol",
        "INSERT INTO probe (id, uk, val, payload) VALUES (1, 'uk-1', 1, 'x')",
        lambda i: None,
        1,
        autocommit=True,
    )
    measure_batch(
        args.dsn,
        "insert_ext_params",
        "INSERT INTO probe (id, uk, val, payload) VALUES (%s, %s, %s, %s)",
        lambda i: (100 + i, f"uk-{100+i}", i, "x" * 40),
        n,
        autocommit=True,
    )
    measure_batch(
        args.dsn,
        "select_ext_params",
        "SELECT id, val, payload FROM probe WHERE id = %s",
        lambda i: (100 + (i % n),),
        n,
        autocommit=True,
    )
    measure_batch(
        args.dsn,
        "update_ext_params",
        "UPDATE probe SET val = val + 1 WHERE id = %s",
        lambda i: (100 + (i % n),),
        n,
        autocommit=True,
    )
    measure_batch(
        args.dsn,
        "delete_ext_params",
        "DELETE FROM probe WHERE id = %s",
        lambda i: (100 + (i % n),),
        1,
        autocommit=True,
    )

    print("\n== explicit transaction, COMMIT each statement ==")
    measure_batch(
        args.dsn,
        "insert_commit_each",
        "INSERT INTO probe (id, uk, val, payload) VALUES (%s, %s, %s, %s)",
        lambda i: (500_000 + i, f"uk-{500_000+i}", i, "x" * 40),
        n,
        autocommit=False,
        commit_each=True,
    )
    measure_batch(
        args.dsn,
        "select_commit_each",
        "SELECT id, val, payload FROM probe WHERE id = %s",
        lambda i: (500_000 + (i % n),),
        n,
        autocommit=False,
        commit_each=True,
    )

    print("\n== concurrency scaling (insert, autocommit) ==")
    for conc in (1, 2, 4, 8):
        wall, lats = concurrency_scaling(args.dsn, conc, args.ops, "insert")
        total = conc * args.ops
        print(
            f"  concurrency={conc} wall={wall*1000:9.1f}ms "
            f"ops={total} throughput={total/wall:8.1f} ops/s "
            f"p50={pct([v*1000 for v in lats],50):7.3f}ms"
        )

    print("\n== concurrency scaling (select, autocommit) ==")
    for conc in (1, 2, 4, 8):
        wall, lats = concurrency_scaling(args.dsn, conc, args.ops, "select")
        total = conc * args.ops
        print(
            f"  concurrency={conc} wall={wall*1000:9.1f}ms "
            f"ops={total} throughput={total/wall:8.1f} ops/s "
            f"p50={pct([v*1000 for v in lats],50):7.3f}ms"
        )


if __name__ == "__main__":
    sys.exit(main())
