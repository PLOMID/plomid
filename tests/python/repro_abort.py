#!/usr/bin/env python3
"""Reproduce the failed-transaction cascade after an unsupported array query."""

import psycopg

DSN = "postgresql://plomid:plomid@127.0.0.1:5432/plomid"


def main():
    conn = psycopg.connect(DSN)  # autocommit=False by default

    # Run a query that fails on the server (array literal not supported)
    try:
        with conn.cursor() as cur:
            cur.execute("SELECT * FROM missing_table_recovery")
            print("array query succeeded (unexpected):", cur.fetchone())
    except Exception as e:
        print(f"array query failed (expected): {type(e).__name__}: {e}")

    # psycopg requires rollback after an error
    try:
        conn.rollback()
        print("rollback OK")
    except Exception as e:
        print(f"rollback failed: {type(e).__name__}: {e}")

    # Now run a simple working query
    try:
        with conn.cursor() as cur:
            cur.execute("SELECT 1")
            print("SELECT 1 result:", cur.fetchone())
    except Exception as e:
        print(f"SELECT 1 after rollback failed: {type(e).__name__}: {e}")

    conn.close()


if __name__ == "__main__":
    main()