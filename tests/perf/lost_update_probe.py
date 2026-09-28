#!/usr/bin/env python3
"""Lost-update / lost-write probe for concurrent single-row UPDATE.

Two shapes, both autocommit:

  disjoint: each worker updates a disjoint id range (independent keys)
  same_row: every worker increments the SAME row N times

For `disjoint` every row must end at exactly one increment. For `same_row`
the row must end at exactly (workers * increments) — anything else is a lost
update.
"""

import sys
import threading

import psycopg2

DSN = sys.argv[1] if len(sys.argv) > 1 else "postgresql://plomid:secret@127.0.0.1:6000/plomid"
SCHEMA = "lostprobe"
INC = 0.000001
TOL = 1e-12


def setup(conn):
    with conn.cursor() as cur:
        cur.execute(f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")
        cur.execute(f"CREATE SCHEMA {SCHEMA}")
        cur.execute(
            f"""
            CREATE TABLE {SCHEMA}.t (
                id BIGINT PRIMARY KEY,
                score DOUBLE PRECISION NOT NULL
            )
            """
        )
    conn.commit()


def fresh(conn, rows):
    with conn.cursor() as cur:
        cur.execute(f"DELETE FROM {SCHEMA}.t")
        cur.execute(
            f"INSERT INTO {SCHEMA}.t (id, score) SELECT g, 0.0 FROM generate_series(1, %s) AS g",
            (rows,),
        )
    conn.commit()


def run_range(conn, wid, ops, base):
    """Each worker updates ids (base + wid*ops + 1 .. base + wid*ops + ops)."""
    errors = []

    def work():
        c = psycopg2.connect(DSN, connect_timeout=30)
        c.autocommit = True
        try:
            for i in range(ops):
                row_id = base + wid * ops + i + 1
                with c.cursor() as cur:
                    cur.execute(
                        f"UPDATE {SCHEMA}.t SET score = score + %s WHERE id = %s",
                        (INC, row_id),
                    )
        except Exception as exc:  # noqa: BLE001
            errors.append(f"w{wid}: {exc}")
        finally:
            c.close()

    return work, errors


def run_same_row(wid, ops, row_id):
    errors = []

    def work():
        c = psycopg2.connect(DSN, connect_timeout=30)
        c.autocommit = True
        try:
            for _ in range(ops):
                with c.cursor() as cur:
                    cur.execute(
                        f"UPDATE {SCHEMA}.t SET score = score + %s WHERE id = %s",
                        (INC, row_id),
                    )
        except Exception as exc:  # noqa: BLE001
            errors.append(f"w{wid}: {exc}")
        finally:
            c.close()

    return work, errors


def launch(workers):
    threads = [threading.Thread(target=fn) for fn in workers]
    for t in threads:
        t.start()
    for t in threads:
        t.join()


def check_range(conn, workers, ops, base, label):
    with conn.cursor() as cur:
        cur.execute(
            f"SELECT count(*), count(*) FILTER (WHERE abs(score - %s) <= %s) "
            f"FROM {SCHEMA}.t WHERE id > %s AND id <= %s",
            (INC, TOL, base, base + workers * ops),
        )
        total, correct = cur.fetchone()
    print(f"  {label}: rows={total} correct={correct} wrong={total - correct} "
          f"-> {'OK' if total == correct else 'FAIL'}")
    if total != correct:
        with conn.cursor() as cur:
            cur.execute(
                f"SELECT id, score FROM {SCHEMA}.t WHERE id > %s AND id <= %s "
                f"AND abs(score - %s) > %s ORDER BY id LIMIT 8",
                (base, base + workers * ops, INC, TOL),
            )
            print(f"    wrong sample: {cur.fetchall()}")
    return total == correct


def main():
    conn = psycopg2.connect(DSN, connect_timeout=30)
    conn.autocommit = True
    setup(conn)

    ok = True
    for workers, ops in ((2, 50), (4, 50), (8, 25)):
        fresh(conn, workers * ops + 1000)
        fns = [run_range(conn, w, ops, 0)[0] for w in range(workers)]
        launch(fns)
        ok &= check_range(conn, workers, ops, 0, f"disjoint c={workers} x {ops}")

    # Same-row contention: no lost updates allowed.
    fresh(conn, 10)
    workers, ops = 4, 25
    fns = [run_same_row(w, ops, 1)[0] for w in range(workers)]
    launch(fns)
    with conn.cursor() as cur:
        cur.execute(f"SELECT score FROM {SCHEMA}.t WHERE id = 1")
        got = cur.fetchone()[0]
    expected = workers * ops * INC
    print(f"  same-row c={workers} x {ops}: got={got!r} expected={expected!r} "
          f"diff={abs(got - expected):.3e} -> "
          f"{'OK' if abs(got - expected) < TOL else 'FAIL (lost updates)'}")
    ok &= abs(got - expected) < TOL

    conn.close()
    print("RESULT:", "ALL OK" if ok else "FAILURES PRESENT")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
