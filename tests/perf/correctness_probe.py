#!/usr/bin/env python3
"""Does an INSERT actually become visible/persistent?

Checks the autocommit + extended-protocol (parameterized) path against a
second connection, plus the literal simple-query path and the explicit
BEGIN/COMMIT path.
"""

import sys

import psycopg2

DSN = sys.argv[1] if len(sys.argv) > 1 else "postgresql://plomid@127.0.0.1:16000/plomid"


def fresh(dsn):
    c = psycopg2.connect(dsn, connect_timeout=30)
    c.autocommit = True
    return c


def visible(dsn, row_id):
    c = psycopg2.connect(dsn, connect_timeout=30)
    c.autocommit = True
    try:
        with c.cursor() as cur:
            cur.execute("SELECT count(*) FROM ct WHERE id = %s", (row_id,))
            return cur.fetchone()[0]
    finally:
        c.close()


def main():
    admin = fresh(DSN)
    with admin.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS ct")
        cur.execute("CREATE TABLE ct (id BIGINT PRIMARY KEY, v INTEGER NOT NULL)")

    # 1. Extended protocol (parameters), autocommit, single implicit statement.
    c = fresh(DSN)
    with c.cursor() as cur:
        cur.execute("INSERT INTO ct (id, v) VALUES (%s, %s)", (1, 10))
    c.close()
    print(f"ext_protocol_autocommit insert id=1 visible_on_other_conn={visible(DSN, 1)}")

    # 2. Literal SQL (simple query protocol), autocommit.
    c = fresh(DSN)
    with c.cursor() as cur:
        cur.execute("INSERT INTO ct (id, v) VALUES (2, 20)")
    c.close()
    print(f"simple_protocol_autocommit insert id=2 visible_on_other_conn={visible(DSN, 2)}")

    # 3. Extended protocol, many rows on one reused connection, autocommit.
    c = fresh(DSN)
    with c.cursor() as cur:
        for i in range(10, 20):
            cur.execute("INSERT INTO ct (id, v) VALUES (%s, %s)", (i, i))
    c.close()
    got = sum(visible(DSN, i) for i in range(10, 20))
    print(f"ext_protocol_autocommit 10 rows on one conn visible={got}/10")

    # 4. Explicit transaction via extended protocol.
    c = psycopg2.connect(DSN, connect_timeout=30)
    c.autocommit = False
    with c.cursor() as cur:
        cur.execute("INSERT INTO ct (id, v) VALUES (%s, %s)", (100, 100))
    c.commit()
    c.close()
    print(f"ext_protocol_explicit_txn insert id=100 visible={visible(DSN, 100)}")

    # 5. Total.
    c = fresh(DSN)
    with c.cursor() as cur:
        cur.execute("SELECT count(*) FROM ct")
        print(f"total_rows={cur.fetchone()[0]} (expected 21)")
    c.close()


if __name__ == "__main__":
    main()
