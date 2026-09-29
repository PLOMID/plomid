#!/usr/bin/env python3
"""Per-statement decomposition probes for PLOMID / PostgreSQL.

Each probe isolates one suspected cost inside the commit pipeline so a
hypothesis can be confirmed or rejected before any code changes:

  insert_scaling   per-statement INSERT latency as the table grows. A cost that
                   is O(rows already in the table) per statement shows up as a
                   linear latency ramp; a cost that is O(row) stays flat.
  commit_only      BEGIN; COMMIT; with no data - isolates WAL barrier cost.
  noindex          INSERT into a table with no PK/UNIQUE - isolates
                   unique-check work from storage work.
  range_select     SELECT rows for 1 / 100 / 1000 / 10000 row tables.

Usage: DB_DSN=... python3 perf_probe.py [probe ...]
"""

import os
import statistics
import sys
import time

import psycopg

DSN = os.getenv("DB_DSN", "postgresql://plomid:plomid@127.0.0.1:5433/plomid")
SCHEMA = os.getenv("TEST_SCHEMA", "perf_probe")


def connect(autocommit=False):
    return psycopg.connect(DSN, autocommit=autocommit, connect_timeout=5)


def percentile(samples, fraction):
    ordered = sorted(samples)
    index = min(len(ordered) - 1, int(len(ordered) * fraction))
    return ordered[index]


def reset(extra_ddl=""):
    with connect(autocommit=True) as conn:
        cur = conn.cursor()
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"CREATE TABLE {SCHEMA}.t (id BIGINT PRIMARY KEY, name TEXT NOT NULL, age INTEGER)"
        )
        cur.execute(
            f"CREATE TABLE {SCHEMA}.t_plain (id BIGINT, name TEXT NOT NULL, age INTEGER)"
        )
        if extra_ddl:
            cur.execute(extra_ddl)


def timed_insert(cur, sql, params):
    start = time.perf_counter()
    cur.execute(sql, params)
    return time.perf_counter() - start


def probe_insert_scaling(count=400, report=50):
    """INSERT one row per transaction; report latency at growing table sizes."""
    reset()
    insert = f"INSERT INTO {SCHEMA}.t (id, name, age) VALUES (%s, %s, %s)"
    windows = []
    samples = []
    with connect() as conn:
        cur = conn.cursor()
        for i in range(count):
            samples.append(timed_insert(cur, insert, (i + 1, f"row-{i}", i % 50)))
            conn.commit()
            if (i + 1) % report == 0:
                windows.append((i + 1, statistics.median(samples[-report:])))
    print(f"  table_size      median_per_stmt_commit")
    for size, latency in windows:
        print(f"  {size:>10d}      {latency * 1000:8.3f} ms")


def probe_commit_only(count=50):
    """Empty transactions: the pure WAL barrier cost with no data."""
    reset()
    samples = []
    with connect() as conn:
        cur = conn.cursor()
        for _ in range(count):
            start = time.perf_counter()
            cur.execute("BEGIN")
            cur.execute("COMMIT")
            samples.append(time.perf_counter() - start)
    print(
        f"  BEGIN;COMMIT;  median={statistics.median(samples) * 1000:.3f} ms  "
        f"min={min(samples) * 1000:.3f} ms"
    )


def probe_noindex(count=200):
    """INSERT into a table with no unique constraint: no unique-check work."""
    reset()
    insert = f"INSERT INTO {SCHEMA}.t_plain (id, name, age) VALUES (%s, %s, %s)"
    samples = []
    with connect() as conn:
        cur = conn.cursor()
        for i in range(count):
            samples.append(timed_insert(cur, insert, (i + 1, f"row-{i}", i % 50)))
            conn.commit()
    print(
        f"  noindex INSERT  median={statistics.median(samples) * 1000:.3f} ms  "
        f"p95={percentile(samples, 0.95) * 1000:.3f} ms"
    )


def probe_range_select():
    """Bulk-load N rows, then time a point lookup and a full scan of N rows."""
    for rows in (1, 100, 1000, 10000):
        reset()
        with connect() as conn:
            cur = conn.cursor()
            payload = [(i + 1, f"row-{i}", i % 50) for i in range(rows)]
            cur.executemany(
                f"INSERT INTO {SCHEMA}.t (id, name, age) VALUES (%s, %s, %s)", payload
            )
            conn.commit()
            point = []
            scan = []
            for _ in range(3):
                start = time.perf_counter()
                cur.execute(f"SELECT * FROM {SCHEMA}.t WHERE id = %s", (rows,))
                cur.fetchall()
                point.append(time.perf_counter() - start)

                start = time.perf_counter()
                cur.execute(f"SELECT * FROM {SCHEMA}.t")
                cur.fetchall()
                scan.append(time.perf_counter() - start)
        print(
            f"  rows={rows:>6d}  point={statistics.median(point) * 1000:7.3f} ms  "
            f"scan={statistics.median(scan) * 1000:8.3f} ms"
        )


def probe_point_lookup():
    """Point lookup with a literal vs a bound parameter, at two table sizes.

    A literal lets the planner see `col = <const>`; a bound parameter is
    substituted server-side. Index use shows up as latency that does not grow
    with table size.
    """
    for rows in (100, 10000):
        reset()
        with connect() as conn:
            cur = conn.cursor()
            cur.executemany(
                f"INSERT INTO {SCHEMA}.t (id, name, age) VALUES (%s, %s, %s)",
                [(i + 1, f"row-{i}", i % 50) for i in range(rows)],
            )
            conn.commit()
            samples_literal = []
            samples_bound = []
            samples_prefix = []
            for _ in range(5):
                start = time.perf_counter()
                cur.execute(f"SELECT * FROM {SCHEMA}.t WHERE id = {rows}")
                cur.fetchall()
                samples_literal.append(time.perf_counter() - start)

                start = time.perf_counter()
                cur.execute(f"SELECT * FROM {SCHEMA}.t WHERE id = %s", (rows,))
                cur.fetchall()
                samples_bound.append(time.perf_counter() - start)

                start = time.perf_counter()
                cur.execute(f"SELECT * FROM {SCHEMA}.t LIMIT 1")
                cur.fetchall()
                samples_prefix.append(time.perf_counter() - start)

        print(
            f"  rows={rows:>6d}  literal={statistics.median(samples_literal) * 1000:7.3f} ms  "
            f"bound={statistics.median(samples_bound) * 1000:7.3f} ms  "
            f"limit1={statistics.median(samples_prefix) * 1000:7.3f} ms"
        )


def probe_multirow_insert():
    """One multi-row INSERT statement: per-row cost without per-statement setup."""
    reset()
    with connect() as conn:
        cur = conn.cursor()
        for rows in (1, 100, 1000, 10000):
            values = ", ".join(f"({i}, 'row-{i}', {i % 50})" for i in range(rows))
            start = time.perf_counter()
            cur.execute(f"INSERT INTO {SCHEMA}.t (id, name, age) VALUES {values}")
            elapsed = time.perf_counter() - start
            conn.commit()
            print(
                f"  rows={rows:>6d}  total={elapsed * 1000:9.3f} ms  "
                f"per_row={elapsed / rows * 1e6:8.1f} us"
            )


PROBES = {
    "insert_scaling": probe_insert_scaling,
    "commit_only": probe_commit_only,
    "noindex": probe_noindex,
    "range_select": probe_range_select,
    "point_lookup": probe_point_lookup,
    "multirow_insert": probe_multirow_insert,
}


def main():
    names = sys.argv[1:] or list(PROBES)
    for name in names:
        probe = PROBES.get(name)
        if probe is None:
            print(f"unknown probe: {name}", file=sys.stderr)
            continue
        print(f"{name}:")
        probe()


if __name__ == "__main__":
    main()
