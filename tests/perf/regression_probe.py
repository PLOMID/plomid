#!/usr/bin/env python3
"""Behavioural regression checks after the latency/concurrency fixes.

Covers: COMMIT/ROLLBACK, unique + primary key enforcement, composite unique,
NULL semantics, UPDATE/DELETE, protocol error responses, connection reuse,
and independent-key concurrency.
"""

import sys
import threading

import psycopg2

DSN = sys.argv[1] if len(sys.argv) > 1 else "postgresql://plomid@127.0.0.1:16000/plomid"

PASS = 0
FAIL = 0


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS  {name}")
    else:
        FAIL += 1
        print(f"  FAIL  {name} {detail}")


def conn(autocommit=True):
    c = psycopg2.connect(DSN, connect_timeout=30)
    c.autocommit = autocommit
    return c


def setup():
    c = conn()
    with c.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS reg")
        cur.execute(
            """
            CREATE TABLE reg (
                id      BIGINT PRIMARY KEY,
                u       BIGINT UNIQUE,
                a       INTEGER,
                b       INTEGER,
                nullable INTEGER,
                UNIQUE (a, b)
            )
            """
        )
    c.close()


def main():
    setup()

    # --- COMMIT persists, ROLLBACK does not -----------------------------
    c = conn(autocommit=False)
    with c.cursor() as cur:
        cur.execute("INSERT INTO reg VALUES (1, 100, 1, 1, NULL)")
    c.commit()
    with c.cursor() as cur:
        cur.execute("SELECT count(*) FROM reg WHERE id = 1")
        check("commit persists", cur.fetchone()[0] == 1)
        cur.execute("UPDATE reg SET u = 999 WHERE id = 1")
    c.rollback()
    with c.cursor() as cur:
        cur.execute("SELECT u FROM reg WHERE id = 1")
        check("rollback discards update", cur.fetchone()[0] == 100)

    # --- PRIMARY KEY enforcement ----------------------------------------
    try:
        with c.cursor() as cur:
            cur.execute("INSERT INTO reg VALUES (1, 200, 2, 2, NULL)")
        c.rollback()
        check("primary key enforced", False, "(duplicate pk accepted)")
    except psycopg2.Error:
        c.rollback()
        check("primary key enforced", True)

    # --- UNIQUE enforcement ---------------------------------------------
    try:
        with c.cursor() as cur:
            cur.execute("INSERT INTO reg VALUES (2, 100, 3, 3, NULL)")
        c.rollback()
        check("unique enforced", False, "(duplicate unique accepted)")
    except psycopg2.Error:
        c.rollback()
        check("unique enforced", True)

    # --- composite UNIQUE -------------------------------------------------
    with c.cursor() as cur:
        cur.execute("INSERT INTO reg VALUES (3, 300, 7, 7, NULL)")
    c.commit()
    try:
        with c.cursor() as cur:
            cur.execute("INSERT INTO reg VALUES (4, 400, 7, 7, NULL)")
        c.rollback()
        check("composite unique enforced", False, "(duplicate tuple accepted)")
    except psycopg2.Error:
        c.rollback()
        check("composite unique enforced", True)

    # --- NULL semantics: multiple NULLs allowed in a UNIQUE column --------
    try:
        with c.cursor() as cur:
            cur.execute("INSERT INTO reg VALUES (5, NULL, 50, 50, NULL)")
            cur.execute("INSERT INTO reg VALUES (6, NULL, 60, 60, NULL)")
        c.commit()
        check("NULLs do not conflict in UNIQUE", True)
    except psycopg2.Error as exc:
        c.rollback()
        check("NULLs do not conflict in UNIQUE", False, str(exc))

    # --- NULL semantics: composite unique with a NULL component ----------
    try:
        with c.cursor() as cur:
            cur.execute("INSERT INTO reg VALUES (7, 700, NULL, 9, NULL)")
            cur.execute("INSERT INTO reg VALUES (8, 800, NULL, 9, NULL)")
        c.commit()
        check("NULL component does not conflict in composite UNIQUE", True)
    except psycopg2.Error as exc:
        c.rollback()
        check("NULL component does not conflict in composite UNIQUE", False, str(exc))

    # --- UPDATE / DELETE (autocommit: immediately visible) ----------------
    c2 = conn()
    with c2.cursor() as cur:
        cur.execute("UPDATE reg SET nullable = 5 WHERE id = 5")
        check("update rowcount", cur.rowcount == 1, f"rowcount={cur.rowcount}")
        cur.execute("SELECT nullable FROM reg WHERE id = 5")
        check("update visible", cur.fetchone()[0] == 5)
        cur.execute("DELETE FROM reg WHERE id = 5")
        check("delete rowcount", cur.rowcount == 1, f"rowcount={cur.rowcount}")
        cur.execute("SELECT count(*) FROM reg WHERE id = 5")
        check("delete removed row", cur.fetchone()[0] == 0)
    c2.close()

    # --- KNOWN PRE-EXISTING DEVIATION (not a regression) ------------------
    # `handle_query` defers INSERT/UPDATE/DELETE to COMMIT ("staged") while a
    # SELECT executes immediately against committed state, so a SELECT inside
    # an explicit transaction does not observe the transaction's own writes.
    # Recorded here so the deviation is visible and tracked; the staged-DML
    # transaction model is outside the scope of this change.
    t = conn(autocommit=False)
    with t.cursor() as cur:
        cur.execute("SELECT count(*) FROM reg")
        before = cur.fetchone()[0]
        cur.execute("INSERT INTO reg VALUES (20000, 20000, 20000, 20000, NULL)")
        cur.execute("SELECT count(*) FROM reg")
        inside = cur.fetchone()[0]
    t.commit()
    with t.cursor() as cur:
        cur.execute("SELECT count(*) FROM reg")
        after = cur.fetchone()[0]
    t.close()
    print(
        f"  NOTE  read-your-own-writes in explicit txn: before={before} "
        f"inside={inside} after_commit={after} "
        f"(pre-existing staged-DML deviation, unchanged by this fix)"
    )

    # --- failed statement does not kill the connection --------------------
    try:
        with c.cursor() as cur:
            cur.execute("INSERT INTO reg VALUES (1, 900, 90, 90, NULL)")
    except psycopg2.Error:
        c.rollback()
    try:
        with c.cursor() as cur:
            cur.execute("SELECT count(*) FROM reg")
            n = cur.fetchone()[0]
        check("connection usable after statement error", n >= 4, f"count={n}")
    except psycopg2.Error as exc:
        check("connection usable after statement error", False, str(exc))
    c.close()

    # --- connection reuse across many statements ---------------------------
    c = conn()
    with c.cursor() as cur:
        for i in range(1000, 1200):
            cur.execute("INSERT INTO reg VALUES (%s, %s, %s, %s, NULL)", (i, i, i, i))
        cur.execute("SELECT count(*) FROM reg WHERE id >= 1000 AND id < 1200")
        check("200 inserts on one connection", cur.fetchone()[0] == 200)
    c.close()

    # --- independent-key concurrency: 8 workers, disjoint keys -------------
    errors = []

    def worker(w):
        try:
            cc = conn()
            with cc.cursor() as cur:
                for i in range(100):
                    row = 5000 + w * 100 + i
                    cur.execute(
                        "INSERT INTO reg VALUES (%s, %s, %s, %s, NULL)",
                        (row, row, row, row),
                    )
                    cur.execute("SELECT nullable FROM reg WHERE id = %s", (row,))
                    cur.fetchone()
            cc.close()
        except Exception as exc:  # noqa: BLE001
            errors.append(f"w{w}: {exc}")

    threads = [threading.Thread(target=worker, args=(w,)) for w in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    check("8 concurrent workers on disjoint keys", not errors, str(errors[:2]))

    c = conn()
    with c.cursor() as cur:
        cur.execute("SELECT count(*) FROM reg WHERE id >= 5000 AND id < 5800")
        check("all 800 concurrent rows committed", cur.fetchone()[0] == 800)
    c.close()

    print(f"\n{PASS} passed, {FAIL} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
