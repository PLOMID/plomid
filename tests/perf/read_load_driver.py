#!/usr/bin/env python3
"""Sustained point-read load driver, for profiling a running PLOMID server.

Seeds a table if needed, then runs `--procs` single-threaded client processes
(one per process, so CPython's GIL never caps the load) for `--seconds`, and
prints aggregate throughput. Designed to be started in the background while a
profiler samples the server process.

Usage:
    python3 tests/perf/read_load_driver.py --port 16400 --seconds 8 --procs 8
"""

import argparse
import multiprocessing as mp
import time

import psycopg2


def seed(dsn, size, batch=2000):
    conn = psycopg2.connect(dsn, connect_timeout=60)
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS load_probe")
        cur.execute(
            "CREATE TABLE load_probe (id BIGINT PRIMARY KEY, k INTEGER NOT NULL, val INTEGER NOT NULL)"
        )
    with conn.cursor() as cur:
        for base in range(0, size, batch):
            cur.execute("BEGIN")
            for i in range(base, min(base + batch, size)):
                cur.execute("INSERT INTO load_probe VALUES (%s,%s,%s)", (i, i % 1000, i))
            cur.execute("COMMIT")
    conn.close()


def worker(dsn, wid, size, seconds, mode, ready, tally):
    conn = psycopg2.connect(dsn, connect_timeout=60)
    conn.autocommit = True
    cur = conn.cursor()
    count = 0
    ready.put(1)
    deadline = time.perf_counter() + seconds
    while time.perf_counter() < deadline:
        if mode == "select1":
            # No storage access at all: parse + plan + execute + wire only.
            cur.execute("SELECT 1")
        elif mode == "count":
            cur.execute("SELECT COUNT(*) FROM load_probe")
        else:
            key = (wid * 7919 + count * 31) % size
            cur.execute("SELECT id,val FROM load_probe WHERE id = %s", (key,))
        cur.fetchall()
        count += 1
    conn.close()
    tally.put(count)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--rows", type=int, default=100000)
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--procs", type=int, default=8)
    ap.add_argument("--mode", default="point", choices=["point", "select1", "count"])
    ap.add_argument("--levels", default="", help="comma-separated procs to sweep")
    ap.add_argument("--seed", action="store_true")
    args = ap.parse_args()

    dsn = f"postgresql://plomid:plomid@{args.host}:{args.port}/plomid"
    if args.seed:
        seed(dsn, args.rows)

    levels = [int(v) for v in args.levels.split(",") if v.strip()] or [args.procs]
    for procs_n in levels:
        ready, tally = mp.Queue(), mp.Queue()
        procs = [
            mp.Process(target=worker, args=(dsn, w, args.rows, args.seconds, args.mode, ready, tally))
            for w in range(procs_n)
        ]
        for p in procs:
            p.start()
        for _ in procs:
            ready.get()
        start = time.perf_counter()
        total = 0
        for _ in procs:
            total += tally.get()
        wall = time.perf_counter() - start
        for p in procs:
            p.join()
        print(
            f"LOAD mode={args.mode} procs={procs_n} seconds={wall:.2f} ops={total} "
            f"throughput={total / wall:.0f}",
            flush=True,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
