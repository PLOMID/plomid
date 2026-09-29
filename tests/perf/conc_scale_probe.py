#!/usr/bin/env python3
"""Concurrency scaling probe.

Measures the benchmark's two concurrent workloads at concurrency 1/2/4/8:

  reads : SELECT ... WHERE id = %s      (PK point lookup, autocommit)
  writes: UPDATE ... SET score = score + 0.000001 WHERE id = %s

Reports ops/sec and p50/p95 latency, then verifies every worker's increments
actually landed (no lost updates under row-level locking).
"""

import math
import statistics
import sys
import threading
import time

import psycopg2

DSN = sys.argv[1] if len(sys.argv) > 1 else "postgresql://plomid:secret@127.0.0.1:6000/plomid"
ROWS = int(sys.argv[2]) if len(sys.argv) > 2 else 20000
OPS_PER_WORKER = int(sys.argv[3]) if len(sys.argv) > 3 else 200
SCHEMA = "concbench"


def pctl(values, p):
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
                status VARCHAR(32) NOT NULL,
                score DOUBLE PRECISION NOT NULL
            )
            """
        )
    conn.commit()
    with conn.cursor() as cur:
        cur.execute(
            f"INSERT INTO {SCHEMA}.events (id, customer_id, amount, status, score) "
            f"SELECT g, g, 1.0, 'active', 0.0 FROM generate_series(1, %s) AS g",
            (ROWS,),
        )
    conn.commit()


def run_reads(concurrency, ops_per_worker):
    lat = []
    errors = []
    lock = threading.Lock()

    def worker(wid):
        conn = psycopg2.connect(DSN, connect_timeout=30)
        conn.autocommit = True
        local = []
        try:
            for i in range(ops_per_worker):
                row_id = ((wid * 7919 + i * 104729) % ROWS) + 1
                start = time.perf_counter()
                with conn.cursor() as cur:
                    cur.execute(
                        f"SELECT id, customer_id, amount, status FROM {SCHEMA}.events WHERE id = %s",
                        (row_id,),
                    )
                    cur.fetchone()
                local.append(time.perf_counter() - start)
        except Exception as exc:  # noqa: BLE001
            with lock:
                errors.append(f"w{wid}: {exc}")
        finally:
            conn.close()
        with lock:
            lat.extend(local)

    threads = [threading.Thread(target=worker, args=(w,)) for w in range(concurrency)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - start
    total = concurrency * ops_per_worker
    return total, wall, total / wall, pctl(lat, 50) * 1000, pctl(lat, 95) * 1000, errors


def run_writes(concurrency, ops_per_worker):
    lat = []
    errors = []
    lock = threading.Lock()

    def worker(wid):
        conn = psycopg2.connect(DSN, connect_timeout=30)
        conn.autocommit = True
        local = []
        try:
            for i in range(ops_per_worker):
                # Disjoint row ranges per worker: independent keys.
                row_id = wid * ops_per_worker + i + 1
                start = time.perf_counter()
                with conn.cursor() as cur:
                    cur.execute(
                        f"UPDATE {SCHEMA}.events SET score = score + 0.000001 WHERE id = %s",
                        (row_id,),
                    )
                local.append(time.perf_counter() - start)
        except Exception as exc:  # noqa: BLE001
            with lock:
                errors.append(f"w{wid}: {exc}")
        finally:
            conn.close()
        with lock:
            lat.extend(local)

    threads = [threading.Thread(target=worker, args=(w,)) for w in range(concurrency)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - start
    total = concurrency * ops_per_worker
    return total, wall, total / wall, pctl(lat, 50) * 1000, pctl(lat, 95) * 1000, errors


def reset_scores(conn):
    """Zero every score so each concurrency level starts from a clean slate.

    Workers at level N rewrite ids 1..N*ops, which earlier levels already
    incremented; without a reset the check would see two increments on those
    rows and misreport a lost update.
    """
    with conn.cursor() as cur:
        cur.execute(f"UPDATE {SCHEMA}.events SET score = 0.0")


def verify_writes(conn, concurrency, ops_per_worker):
    expected = 0.000001
    with conn.cursor() as cur:
        cur.execute(
            f"SELECT count(*) FROM {SCHEMA}.events "
            f"WHERE id <= %s AND abs(score - %s) > 1e-12",
            (concurrency * ops_per_worker, expected),
        )
        bad = cur.fetchone()[0]
        cur.execute(
            f"SELECT count(*) FROM {SCHEMA}.events WHERE id > %s AND score <> 0",
            (concurrency * ops_per_worker,),
        )
        untouched = cur.fetchone()[0]
    return bad, untouched


def main():
    conn = psycopg2.connect(DSN, connect_timeout=30)
    conn.autocommit = True
    setup(conn)

    print(f"scale: rows={ROWS} ops_per_worker={OPS_PER_WORKER}")
    print(f"{'workload':<8} {'conc':>4} {'ops':>6} {'wall_s':>8} {'ops/sec':>10} "
          f"{'p50_ms':>8} {'p95_ms':>8} {'errors':>7}")

    for conc in (1, 2, 4, 8):
        total, wall, rate, p50, p95, errs = run_reads(conc, OPS_PER_WORKER)
        print(f"{'reads':<8} {conc:>4} {total:>6} {wall:>8.3f} {rate:>10.1f} "
              f"{p50:>8.3f} {p95:>8.3f} {len(errs):>7}")
        if errs:
            print(f"         first error: {errs[0]}")

    for conc in (1, 2, 4, 8):
        reset_scores(conn)
        total, wall, rate, p50, p95, errs = run_writes(conc, OPS_PER_WORKER)
        print(f"{'writes':<8} {conc:>4} {total:>6} {wall:>8.3f} {rate:>10.1f} "
              f"{p50:>8.3f} {p95:>8.3f} {len(errs):>7}")
        if errs:
            print(f"         first error: {errs[0]}")
        bad, untouched = verify_writes(conn, conc, OPS_PER_WORKER)
        print(f"         correctness: rows_wrong={bad} untouched_nonzero={untouched} "
              f"-> {'OK' if bad == 0 and untouched == 0 else 'FAIL'}")
        if bad or untouched:
            break

    conn.close()


if __name__ == "__main__":
    main()
