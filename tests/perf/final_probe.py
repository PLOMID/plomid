#!/usr/bin/env python3
"""PLOMID final performance probe: scale matrix, tail latency, concurrency, HTAP.

Every number printed here is measured against a live PLOMID server over the
PGWire protocol. Lines beginning with ``RESULT`` are the machine-readable
records used in the report; nothing is estimated or extrapolated.

Usage:
    python3 tests/perf/final_probe.py \\
        --dsn postgresql://plomid:plomid@127.0.0.1:16000/plomid
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


def result(name, **fields):
    print("RESULT " + name + " " + " ".join(f"{k}={v}" for k, v in fields.items()), flush=True)


def connect(dsn):
    return psycopg2.connect(dsn, connect_timeout=60)


def build(dsn, size, batch=2000):
    """Create and fill the probe table with `size` rows, one txn per batch."""
    conn = connect(dsn)
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS probe_final")
        cur.execute(
            """
            CREATE TABLE probe_final (
                id      BIGINT PRIMARY KEY,
                k       INTEGER NOT NULL,
                val     INTEGER NOT NULL,
                tag     VARCHAR(24),
                payload TEXT
            )
            """
        )
        cur.execute("CREATE INDEX probe_final_k_idx ON probe_final (k)")
    start = time.perf_counter()
    with conn.cursor() as cur:
        for base in range(0, size, batch):
            cur.execute("BEGIN")
            for i in range(base, min(base + batch, size)):
                cur.execute(
                    "INSERT INTO probe_final VALUES (%s,%s,%s,%s,%s)",
                    (i, i % 1000, i * 3 % 100000, f"t{i % 50}", "x" * 32),
                )
            cur.execute("COMMIT")
    elapsed = time.perf_counter() - start
    conn.close()
    result(
        "insert",
        rows=size,
        seconds=f"{elapsed:.3f}",
        rows_per_sec=f"{size / elapsed:.0f}",
    )


def bench(cur, label, sql, params, iters, expected=None):
    """Run one query `iters` times; report latency percentiles."""
    lats = []
    returned = 0
    for i in range(iters):
        start = time.perf_counter()
        cur.execute(sql, params(i))
        if cur.description is not None:
            returned = len(cur.fetchall())
        lats.append(time.perf_counter() - start)
    ms = [v * 1000 for v in lats]
    verdict = ""
    if expected is not None:
        verdict = "OK" if returned == expected else f"WRONG(expected {expected})"
    result(
        label,
        returned=returned,
        n=iters,
        p50_ms=f"{pct(ms, 50):.3f}",
        p95_ms=f"{pct(ms, 95):.3f}",
        p99_ms=f"{pct(ms, 99):.3f}",
        mean_ms=f"{statistics.mean(ms):.3f}",
        verdict=verdict,
    )
    return returned


def scale_block(dsn, size, iters, heavy):
    """OLTP + analytical measurements at one dataset size."""
    print(f"\n=== size {size} ===", flush=True)
    build(dsn, size)
    conn = connect(dsn)
    conn.autocommit = True
    cur = conn.cursor()
    n = iters
    bench(cur, "pk_eq", "SELECT id,val FROM probe_final WHERE id = %s",
          lambda i: ((i * 7919) % size,), n, expected=1)
    bench(cur, "index_eq", "SELECT id,val FROM probe_final WHERE k = %s",
          lambda i: (i % 1000,), n, expected=size // 1000)
    bench(cur, "index_range", "SELECT id,val FROM probe_final WHERE k BETWEEN %s AND %s",
          lambda i: (100, 110), n, expected=11 * (size // 1000))
    bench(cur, "pk_range", "SELECT id,val FROM probe_final WHERE id BETWEEN %s AND %s",
          lambda i: (100, 200), n, expected=min(101, size))
    bench(cur, "update_pk", "UPDATE probe_final SET val = val + 1 WHERE id = %s",
          lambda i: ((i * 7919) % size,), min(n, 100))
    if heavy:
        bench(cur, "count_star", "SELECT COUNT(*) FROM probe_final", lambda i: (), 5,
              expected=1)
        bench(cur, "sum_val", "SELECT SUM(val) FROM probe_final", lambda i: (), 5)
        bench(cur, "group_by_k", "SELECT k, COUNT(*) FROM probe_final GROUP BY k",
              lambda i: (), 3)
        bench(cur, "order_by_limit", "SELECT id,val FROM probe_final ORDER BY val DESC LIMIT 10",
              lambda i: (), 3, expected=10)
        bench(cur, "nonindexed_eq", "SELECT id,val FROM probe_final WHERE tag = %s",
              lambda i: (f"t{i % 50}",), 3, expected=size // 50)
    conn.close()


def concurrency_block(dsn, size, levels, ops):
    """Point-read and insert throughput + tail latency vs concurrent users.

    Every insert uses a key that is unique across the *whole* run, not just
    within one level. Deriving keys from `(worker, sequence)` alone silently
    re-inserts the previous level's rows at every higher level, which shows up
    as duplicate-key errors that look like a server bug but are the harness
    colliding with its own data.
    """
    level_ordinal = 0
    for mode in ("read", "insert"):
        for users in levels:
            lats = []
            errors = []
            lock = threading.Lock()
            level_base = size + 1_000_000_000 + level_ordinal * 10_000_000
            level_ordinal += 1

            def worker(wid):
                try:
                    conn = connect(dsn)
                    conn.autocommit = True
                    local = []
                    with conn.cursor() as cur:
                        for i in range(ops):
                            key = (wid * ops + i) % size
                            start = time.perf_counter()
                            if mode == "read":
                                cur.execute(
                                    "SELECT id,val FROM probe_final WHERE id = %s", (key,)
                                )
                            else:
                                cur.execute("BEGIN")
                                cur.execute(
                                    "INSERT INTO probe_final VALUES (%s,%s,%s,%s,%s)",
                                    (level_base + wid * ops + i, i % 1000, i, "t", "x" * 32),
                                )
                                cur.execute("COMMIT")
                            local.append(time.perf_counter() - start)
                    conn.close()
                    with lock:
                        lats.extend(local)
                except Exception as exc:  # record, never hide
                    with lock:
                        errors.append(repr(exc))

            threads = [threading.Thread(target=worker, args=(w,)) for w in range(users)]
            start = time.perf_counter()
            for t in threads:
                t.start()
            for t in threads:
                t.join()
            wall = time.perf_counter() - start
            total = users * ops
            ms = [v * 1000 for v in lats]
            result(
                f"conc_{mode}",
                users=users,
                ops=total,
                wall_ms=f"{wall * 1000:.1f}",
                throughput=f"{total / wall:.0f}",
                p50_ms=f"{pct(ms, 50):.3f}",
                p95_ms=f"{pct(ms, 95):.3f}",
                p99_ms=f"{pct(ms, 99):.3f}",
                max_ms=f"{max(ms) if ms else 0:.3f}",
                errors=len(errors),
            )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dsn", required=True)
    ap.add_argument("--sizes", default="1000,10000,100000")
    ap.add_argument("--iters", type=int, default=200)
    ap.add_argument("--conc-size", type=int, default=100000)
    ap.add_argument("--conc-levels", default="1,2,4,8,16")
    ap.add_argument("--conc-ops", type=int, default=60)
    ap.add_argument("--skip-scale", action="store_true")
    ap.add_argument("--skip-conc", action="store_true")
    args = ap.parse_args()

    if not args.skip_scale:
        for size in [int(s) for s in args.sizes.split(",") if s.strip()]:
            # Scans grow linearly with the table, so only measure the heavy
            # analytical set once the table is large enough to be meaningful.
            scale_block(args.dsn, size, args.iters, heavy=size >= 100000)

    if not args.skip_conc:
        print("\n=== concurrency ===", flush=True)
        build(args.dsn, args.conc_size)
        concurrency_block(
            args.dsn,
            args.conc_size,
            [int(l) for l in args.conc_levels.split(",") if l.strip()],
            args.conc_ops,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
