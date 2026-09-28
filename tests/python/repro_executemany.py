#!/usr/bin/env python3
"""Reproduce the executemany() failure."""

import psycopg

DSN = "postgresql://plomid:plomid@127.0.0.1:5432/plomid"


def main():
    conn = psycopg.connect(DSN, autocommit=True)
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS test_schema.users")
        cur.execute("DROP SCHEMA IF EXISTS test_schema")
        cur.execute("CREATE SCHEMA test_schema")
        cur.execute("""
            CREATE TABLE test_schema.users (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )
        """)
    conn.close()

    # Now with autocommit=False (transaction mode), like the test
    conn = psycopg.connect(DSN, autocommit=False)
    try:
        # First do a plain INSERT commit to ensure clean state
        with conn.cursor() as cur:
            cur.execute("INSERT INTO test_schema.users (id, name) VALUES (1, 'Alice')")
        conn.commit()
        print("plain INSERT + COMMIT OK")

        # Now executemany
        try:
            with conn.cursor() as cur:
                cur.executemany(
                    "INSERT INTO test_schema.users (id, name) VALUES (%s, %s)",
                    [(2, "Bob"), (3, "Charlie"), (4, "David")],
                )
            conn.commit()
            print("executemany + COMMIT OK")

            with conn.cursor() as cur:
                cur.execute("SELECT COUNT(*) FROM test_schema.users")
                print("row count:", cur.fetchone()[0])
        except Exception as e:
            print(f"executemany error: {type(e).__name__}: {e}")
    finally:
        try:
            conn.rollback()
        except Exception:
            pass
        conn.close()


if __name__ == "__main__":
    main()