#!/usr/bin/env python3
"""Reproduce the duplicate INSERT issue."""

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

    # Now test with autocommit OFF (transaction mode)
    conn = psycopg.connect(DSN, autocommit=False)
    try:
        with conn.cursor() as cur:
            print("Executing INSERT...")
            cur.execute(
                "INSERT INTO test_schema.users (id, name) VALUES (%s, %s)",
                (1, "Alice"),
            )
            print(f"rowcount after INSERT: {cur.rowcount}")

        # Commit the transaction
        print("Committing transaction...")
        conn.commit()
        print("Transaction committed")

        # Verify the data
        with conn.cursor() as cur:
            cur.execute("SELECT * FROM test_schema.users")
            rows = cur.fetchall()
            print(f"Rows in table: {rows}")
    finally:
        try:
            conn.rollback()
        except Exception:
            pass
        conn.close()

    # Cleanup
    conn = psycopg.connect(DSN, autocommit=True)
    with conn.cursor() as cur:
        cur.execute("DROP TABLE IF EXISTS test_schema.users")
        cur.execute("DROP SCHEMA IF EXISTS test_schema")
    conn.close()


if __name__ == "__main__":
    main()
