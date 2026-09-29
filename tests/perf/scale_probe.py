#!/usr/bin/env python3
"""PLOMID scale + path forensics probe.

Measures, against a live PLOMID server over the PGWire protocol:

  * bulk INSERT throughput (batched, one transaction per batch)
  * primary-key point SELECT scaling (1K -> 10K -> 100K -> 1M)
  * indexed range SELECT
  * full-table COUNT(*)
  * UPDATE / DELETE by primary key
  * storage footprint of the data directory

Every measurement prints a single machine-readable line prefixed with
``RESULT`` so the numbers land in a report without transcription.

Usage:
    python3 tests/perf/scale_probe.py --dsn postgresql://plomid:plomid@127.0.0.1:16000/plomid \
        --sizes 1000,10000,100000 --point-iters 300
"""

import argparse
import os
import statistics
import sys
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
    body = " ".join(f"{k}={v}" for k, v in fields.items())
    print(f"RESULT {name} {body}", flush=True)


def connect(dsn):
    return psycopg2.connect(dsn, connect_timeout=60)


def dir_size(path):
    total = 0
    files = 0
    for root, _dirs, names in os.walk(path):
        for n in names:
            fp = os.path.join(root, n)
            try:
                total += os.path.getsize(fp)
                files += 1
            except OSError:
                pass
    return total, files


def setup(dsn, size):
    conn = connect(dsn)
    conn.autocommit = True
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS scale_probe")
        cur.execute(
            """
            CREATE TABLE scale_probe (
                id      BIGINT PRIMARY KEY,
                k       INTEGER NOT NULL,
                val     INTEGER NOT NULL,
                tag     VARCHAR(24),
                payload TEXT
            )
            """
        )
        cur.execute("CREATE INDEX scale_probe_k_idx ON scale_probe (k)")
    conn.close()

    # One explicit transaction per batch: committing per row would measure
    # fsync latency rather than insert throughput, and would not reflect how a
    # bulk load is actually driven.
    conn = connect(dsn)
    batch = 2000
    start = time.perf_counter()
    with conn.cursor() as cur:
        for base in range(0, size, batch):
            cur.execute("BEGIN")
            for i in range(base, min(base + batch, size)):
                cur.execute(
                    "INSERT INTO scale_probe (id, k, val, tag, payload) VALUES (%s,%s,%s,%s,%s)",
                    (i, i % 1000, i * 3 % 100000, f"t{i % 50}", "x" * 32),
                )
            cur.execute("COMMIT")
    elapsed = time.perf_counter() - start
    conn.close()
    result(
        "bulk_insert",
        rows=size,
        seconds=f"{elapsed:.3f}",
        rows_per_sec=f"{size / elapsed:.0f}" if elapsed else "inf",
    )


def point_scaling(dsn, size, iters):
    conn = connect(dsn)
    conn.autocommit = True
    lats = []
    with conn.cursor() as cur:
        for i in range(iters):
            key = (i * 7919) % size
            start = time.perf_counter()
            cur.execute(
                "SELECT id, k, val, tag FROM scale_probe WHERE id = %s", (key,)
            )
            cur.fetchall()
            lats.append(time.perf_counter() - start)
    conn.close()
    ms = [v * 1000 for v in lats]
    result(
        "pk_point_select",
        rows=size,
        n=iters,
        p50_ms=f"{pct(ms, 50):.3f}",
        p95_ms=f"{pct(ms, 95):.3f}",
        p99_ms=f"{pct(ms, 99):.3f}",
        mean_ms=f"{statistics.mean(ms):.3f}",
    )


def range_select(dsn, size, iters):
    conn = connect(dsn)
    conn.autocommit = True
    lats = []
    rows_seen = 0
    with conn.cursor() as cur:
        for i in range(iters):
            lo = (i * 977) % max(size - 100, 1)
            start = time.perf_counter()
            cur.execute(
                "SELECT id, val FROM scale_probe WHERE k = %s AND id BETWEEN %s AND %s",
                (i % 1000, lo, lo + 100),
            )
            rows_seen += len(cur.fetchall())
            lats.append(time.perf_counter() - start)
    conn.close()
    ms = [v * 1000 for v in lats]
    result(
        "indexed_range_select",
        rows=size,
        n=iters,
        returned=rows_seen,
        p50_ms=f"{pct(ms, 50):.3f}",
        p95_ms=f"{pct(ms, 95):.3f}",
        mean_ms=f"{statistics.mean(ms):.3f}",
    )


def full_scan(dsn, size, iters):
    conn = connect(dsn)
    conn.autocommit = True
    lats = []
    with conn.cursor() as cur:
        for _ in range(iters):
            start = time.perf_counter()
            cur.execute("SELECT COUNT(*) FROM scale_probe")
            cur.fetchall()
            lats.append(time.perf_counter() - start)
    conn.close()
    ms = [v * 1000 for v in lats]
    result(
        "count_star",
        rows=size,
        n=iters,
        p50_ms=f"{pct(ms, 50):.3f}",
        mean_ms=f"{statistics.mean(ms):.3f}",
        rows_per_sec=f"{size / statistics.mean(lats):.0f}" if lats else "0",
    )


def projection_scan(dsn, size, iters):
    """Analytical scan of two columns over the whole table."""
    conn = connect(dsn)
    conn.autocommit = True
    lats = []
    with conn.cursor() as cur:
        for _ in range(iters):
            start = time.perf_counter()
            cur.execute("SELECT SUM(val) FROM scale_probe WHERE k >= 0")
            cur.fetchall()
            lats.append(time.perf_counter() - start)
    conn.close()
    ms = [v * 1000 for v in lats]
    result(
        "analytical_sum",
        rows=size,
        n=iters,
        p50_ms=f"{pct(ms, 50):.3f}",
        mean_ms=f"{statistics.mean(ms):.3f}",
        rows_per_sec=f"{size / statistics.mean(lats):.0f}" if lats else "0",
    )


def update_delete(dsn, size, iters):
    conn = connect(dsn)
    conn.autocommit = True
    upd = []
    with conn.cursor() as cur:
        for i in range(iters):
            key = (i * 7919) % size
            start = time.perf_counter()
            cur.execute(
                "UPDATE scale_probe SET val = val + 1 WHERE id = %s", (key,)
            )
            upd.append(time.perf_counter() - start)
    conn.close()
    ms = [v * 1000 for v in upd]
    result(
        "pk_update",
        rows=size,
        n=iters,
        p50_ms=f"{pct(ms, 50):.3f}",
        p95_ms=f"{pct(ms, 95):.3f}",
        mean_ms=f"{statistics.mean(ms):.3f}",
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dsn", required=True)
    ap.add_argument("--sizes", default="1000,10000,100000")
    ap.add_argument("--point-iters", type=int, default=300)
    ap.add_argument("--scan-iters", type=int, default=5)
    ap.add_argument("--data-dir", default="")
    ap.add_argument("--skip-insert", action="store_true")
    args = ap.parse_args()

    sizes = [int(s) for s in args.sizes.split(",") if s.strip()]
    for size in sizes:
        print(f"\n=== size {size} ===", flush=True)
        if not args.skip_insert:
            setup(args.dsn, size)
        point_scaling(args.dsn, size, args.point_iters)
        range_select(args.dsn, size, min(50, args.point_iters))
        update_delete(args.dsn, size, min(100, args.point_iters))
        full_scan(args.dsn, size, args.scan_iters)
        projection_scan(args.dsn, size, args.scan_iters)
        if args.data_dir:
            total, files = dir_size(args.data_dir)
            result(
                "storage_footprint",
                rows=size,
                bytes=total,
                files=files,
                bytes_per_row=f"{total / size:.2f}" if size else "0",
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
