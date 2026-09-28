#!/usr/bin/env python3
"""JOIN performance + correctness probe.

Reproduces the benchmark's JOIN shape at the benchmark's default scale
(10,000 events, 100,000 customers) and reports:
  * p50/p95/min/max latency for the selective equi-join
  * result cardinality and row content (correctness)
  * a no-match seek, and a non-indexed join (fallback path)
"""

import statistics
import sys
import time

import psycopg2
from psycopg2.extras import execute_values

DSN = sys.argv[1] if len(sys.argv) > 1 else "postgresql://plomid@127.0.0.1:6000/plomid"
ROWS = int(sys.argv[2]) if len(sys.argv) > 2 else 10000
CUSTOMERS = int(sys.argv[3]) if len(sys.argv) > 3 else 100000
QUERIES = int(sys.argv[4]) if len(sys.argv) > 4 else 100

SCHEMA = "joinbench"


def pct(values, p):
    if not values:
        return 0.0
    ordered = sorted(values)
    pos = (len(ordered) - 1) * (p / 100.0)
    lo = int(pos)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (pos - lo)


def setup(conn):
    with conn.cursor() as cur:
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"""
            CREATE TABLE {SCHEMA}.events (
                id BIGINT PRIMARY KEY,
                customer_id BIGINT NOT NULL,
                amount NUMERIC(14,2) NOT NULL,
                status VARCHAR(32) NOT NULL
            )
            """
        )
        cur.execute(
            f"""
            CREATE TABLE {SCHEMA}.customers (
                customer_id BIGINT PRIMARY KEY,
                customer_name VARCHAR(128) NOT NULL,
                region VARCHAR(32) NOT NULL
            )
            """
        )
    conn.commit()
    with conn.cursor() as cur:
        execute_values(
            cur,
            f"INSERT INTO {SCHEMA}.customers (customer_id, customer_name, region) VALUES %s",
            [(i, f"customer-{i}", ["US", "EU", "APAC", "LATAM", "MEA"][i % 5])
             for i in range(1, CUSTOMERS + 1)],
            page_size=5000,
        )
    conn.commit()
    with conn.cursor() as cur:
        execute_values(
            cur,
            f"INSERT INTO {SCHEMA}.events (id, customer_id, amount, status) VALUES %s",
            [(i, ((i - 1) % CUSTOMERS) + 1, ((i * 17) % 100000) / 100.0, "active")
             for i in range(1, ROWS + 1)],
            page_size=5000,
        )
    conn.commit()
    with conn.cursor() as cur:
        cur.execute(f"CREATE INDEX idx_events_customer_id ON {SCHEMA}.events(customer_id)")
        cur.execute(f"CREATE INDEX idx_events_status ON {SCHEMA}.events(status)")
    conn.commit()


JOIN_SQL = f"""
    SELECT e.id, e.amount, c.customer_name, c.region
    FROM {SCHEMA}.events e
    JOIN {SCHEMA}.customers c ON e.customer_id = c.customer_id
    WHERE e.id = %s
"""


def main():
    conn = psycopg2.connect(DSN, connect_timeout=30)
    conn.autocommit = True
    setup(conn)

    print(f"scale: events={ROWS} customers={CUSTOMERS} queries={QUERIES}")
    lat = []
    with conn.cursor() as cur:
        for i in range(1, QUERIES + 1):
            start = time.perf_counter()
            cur.execute(JOIN_SQL, (i,))
            rows = cur.fetchall()
            lat.append(time.perf_counter() - start)
    ms = [v * 1000 for v in lat]
    print(
        f"selective equi-join: p50={pct(ms,50):8.3f}ms p95={pct(ms,95):8.3f}ms "
        f"min={min(ms):8.3f}ms max={max(ms):8.3f}ms mean={statistics.mean(ms):8.3f}ms"
    )

    # Correctness: exact content for a known row. Values are stringified so the
    # comparison is about the data, not psycopg2's Decimal wrapper.
    with conn.cursor() as cur:
        cur.execute(JOIN_SQL, (5,))
        got = [[str(v) for v in row] for row in cur.fetchall()]
    expected = [["5", "0.85", "customer-5", "US"]]
    print(f"  content id=5: {got} expected {expected} -> {'OK' if got == expected else 'MISMATCH'}")

    with conn.cursor() as cur:
        cur.execute(JOIN_SQL, (ROWS + 1000,))
        print(f"  no-match id={ROWS+1000}: {cur.fetchall()} (expect [])")

    # Row count over a range of ids must equal 1 row per existing id.
    with conn.cursor() as cur:
        cur.execute(
            f"""
            SELECT count(*)
            FROM {SCHEMA}.events e
            JOIN {SCHEMA}.customers c ON e.customer_id = c.customer_id
            WHERE e.id BETWEEN 1 AND 500
            """
        )
        n = cur.fetchone()[0]
    print(f"  range join count(1..500) = {n} (expect 500) -> {'OK' if n == 500 else 'MISMATCH'}")

    # Non-indexed join shape (ON on the non-indexed amount column) must still work.
    with conn.cursor() as cur:
        cur.execute(
            f"""
            SELECT count(*)
            FROM {SCHEMA}.events e
            JOIN {SCHEMA}.customers c ON e.amount = c.customer_id::numeric
            WHERE e.id BETWEEN 1 AND 50
            """
        )
        n = cur.fetchone()[0]
    print(f"  non-indexed ON count = {n} (fallback path, no assertion)")

    # Independent-connection visibility of the join.
    other = psycopg2.connect(DSN, connect_timeout=30)
    other.autocommit = True
    with other.cursor() as cur:
        cur.execute(JOIN_SQL, (7,))
        print(f"  other connection id=7: {cur.fetchall()}")
    other.close()

    conn.close()


if __name__ == "__main__":
    main()
