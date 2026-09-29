#!/usr/bin/env python3
"""Focused concurrency + logging-overhead probe for PLOMID.

Runs one controlled workload and reports wall time / throughput so the same
workload can be compared across server configurations (log level, code
revision).  The server-side group-commit distribution is read from the
server log by the caller.

Usage:
    python3 tests/perf/conc_probe.py --dsn ... --mode write --conc 8 --ops 100
    python3 tests/perf/conc_probe.py --dsn ... --mode read  --conc 8 --ops 100
"""

import argparse
import statistics
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


def setup(dsn, reset=False):
    conn = psycopg2.connect(dsn, connect_timeout=30)
    conn.autocommit = True
    with conn.cursor() as cur:
        if reset:
            cur.execute("DROP TABLE IF EXISTS probe")
            cur.execute(
                """
                CREATE TABLE probe (
                    id      BIGINT PRIMARY KEY,
                    uk      VARCHAR(64) UNIQUE,
                    val     INTEGER NOT NULL,
                    payload TEXT
                )
                """
            )
        # Seed rows used by the read workload.
        cur.execute("SELECT COUNT(*) FROM probe")
    conn.close()


def run(dsn, mode, concurrency, ops, base_id):
    latencies = []
    lock = threading.Lock()

    def worker(worker_id):
        conn = psycopg2.connect(dsn, connect_timeout=30)
        conn.autocommit = True
        local = []
        with conn.cursor() as cur:
            for i in range(ops):
                row_id = base_id + worker_id * ops + i
                start = time.perf_counter()
                if mode == "write":
                    cur.execute(
                        "INSERT INTO probe (id, uk, val, payload) "
                        "VALUES (%s, %s, %s, %s)",
                        (row_id, f"uk-{row_id}", i, "x" * 40),
                    )
                elif mode == "update":
                    cur.execute("UPDATE probe SET val = val + 1 WHERE id = %s", (row_id,))
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
    ms = [v * 1000 for v in latencies]
    return wall, ms


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dsn", default="postgresql://plomid@127.0.0.1:16000/plomid")
    ap.add_argument("--mode", choices=["write", "read", "update"], default="write")
    ap.add_argument("--conc", type=int, default=8)
    ap.add_argument("--ops", type=int, default=100)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--base-id", type=int, default=10_000_000)
    ap.add_argument("--setup", action="store_true")
    args = ap.parse_args()

    if args.setup:
        setup(args.dsn, reset=True)

    walls = []
    all_ms = []
    for rep in range(args.reps):
        base = args.base_id + rep * 1_000_000
        wall, ms = run(args.dsn, args.mode, args.conc, args.ops, base)
        walls.append(wall)
        all_ms.extend(ms)
        ops_total = args.conc * args.ops
        print(
            f"  rep{rep}: wall={wall*1000:9.1f}ms ops={ops_total} "
            f"throughput={ops_total/wall:8.1f} ops/s "
            f"p50={pct(ms,50):8.3f}ms p95={pct(ms,95):8.3f}ms"
        )
    ops_total = args.conc * args.ops
    print(
        f"RESULT mode={args.mode} conc={args.conc} ops={args.ops} "
        f"median_wall_ms={statistics.median(walls)*1000:.1f} "
        f"throughput={ops_total/statistics.median(walls):.1f} "
        f"p50={pct(all_ms,50):.3f} p95={pct(all_ms,95):.3f}"
    )


if __name__ == "__main__":
    main()
