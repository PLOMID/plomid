#!/usr/bin/env python3
"""
PostgreSQL / PostgreSQL-compatible database production test suite.

Designed to test:
    - PostgreSQL
    - PostgreSQL-compatible databases such as Plomid

Environment variables:

    DB_DSN
        PostgreSQL connection string.

    TEST_SCHEMA
        Schema used by the test suite.

    WORKERS
        Number of concurrent workers.

    STRESS_ROWS
        Number of rows used by stress tests.

Example:

    DB_DSN="postgresql://user:password@127.0.0.1:5432/testdb" \
    python postgres_production_test.py

WARNING:
    This test suite creates and destroys objects inside TEST_SCHEMA.

    NEVER point it at a production database.
"""

import os
import sys
import time
import traceback
import threading

from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import date, datetime, timezone
from decimal import Decimal

import psycopg
from psycopg import errors


# ============================================================
# CONFIGURATION
# ============================================================

DB_DSN = os.getenv(
    "DB_DSN",
    "postgresql://plomid:plomid@127.0.0.1:6000/plomid",
    # "postgresql://127.0.0.1:5432/postgres",
    
)

TEST_SCHEMA = os.getenv(
    "TEST_SCHEMA",
    "production_test",
)

WORKERS = int(
    os.getenv("WORKERS", "8")
)

STRESS_ROWS = int(
    os.getenv("STRESS_ROWS", "200")
)

CONNECT_TIMEOUT = 5


# ============================================================
# TEST STATE
# ============================================================

PASSED = 0
FAILED = 0
SKIPPED = 0

RESULTS = []

LOCK = threading.Lock()


# ============================================================
# OUTPUT
# ============================================================

def title(text):
    print()
    print("=" * 80)
    print(text)
    print("=" * 80)


def section(text):
    print()
    print("-" * 80)
    print(text)
    print("-" * 80)


# ============================================================
# RESULT RECORDING
# ============================================================

def record_result(
    name,
    status,
    error=None,
    elapsed=0.0,
):
    global PASSED, FAILED, SKIPPED

    with LOCK:
        RESULTS.append(
            {
                "name": name,
                "status": status,
                "error": error,
                "elapsed": elapsed,
            }
        )

        if status == "PASS":
            PASSED += 1

        elif status == "FAIL":
            FAILED += 1

        elif status == "SKIP":
            SKIPPED += 1


class SkipTest(Exception):
    pass


def run_test(name, function):
    """
    Execute one test and record its execution time.
    """

    print(f"\n[TEST] {name}")

    start = time.perf_counter()

    try:
        function()

    except SkipTest as exc:

        elapsed = time.perf_counter() - start

        print(
            f"       ⚠ SKIP: {exc} "
            f"[{elapsed:.3f}s]"
        )

        record_result(
            name,
            "SKIP",
            str(exc),
            elapsed,
        )

        return False

    except Exception as exc:

        elapsed = time.perf_counter() - start

        print(
            f"       ✗ FAIL: "
            f"{type(exc).__name__}: {exc} "
            f"[{elapsed:.3f}s]"
        )

        record_result(
            name,
            "FAIL",
            f"{type(exc).__name__}: {exc}",
            elapsed,
        )

        return False

    elapsed = time.perf_counter() - start

    print(
        f"       ✓ PASS [{elapsed:.3f}s]"
    )

    record_result(
        name,
        "PASS",
        elapsed=elapsed,
    )

    return True


# ============================================================
# ASSERTIONS
# ============================================================

def check(condition, message):
    if not condition:
        raise AssertionError(message)


def check_equal(actual, expected, message):
    if actual != expected:
        raise AssertionError(
            f"{message}: expected={expected!r}, "
            f"actual={actual!r}"
        )


# ============================================================
# CONNECTION
# ============================================================

def connect():
    return psycopg.connect(
        DB_DSN,
        connect_timeout=CONNECT_TIMEOUT,
    )


def test_connection():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute("SELECT 1")

            check_equal(
                cur.fetchone(),
                (1,),
                "SELECT 1",
            )


def test_connection_metadata():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT current_database()"
            )

            database = cur.fetchone()[0]

            cur.execute(
                "SELECT current_schema()"
            )

            schema = cur.fetchone()[0]

            cur.execute(
                "SELECT version()"
            )

            version = cur.fetchone()[0]

            print(
                f"       database: {database}"
            )

            print(
                f"       schema:   {schema}"
            )

            print(
                f"       version:  {version}"
            )

            check(
                database is not None,
                "Database missing",
            )

            check(
                version is not None,
                "Version missing",
            )


# ============================================================
# TEST SCHEMA
# ============================================================

def create_schema():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                CREATE SCHEMA IF NOT EXISTS
                {TEST_SCHEMA}
                """
            )

        conn.commit()


def cleanup_schema():

    try:

        with connect() as conn:

            with conn.cursor() as cur:

                cur.execute(
                    f"""
                    DROP SCHEMA IF EXISTS
                    {TEST_SCHEMA}
                    CASCADE
                    """
                )

            conn.commit()

    except Exception as exc:

        print(
            f"Cleanup failed: "
            f"{type(exc).__name__}: {exc}"
        )


# ============================================================
# BASIC SQL
# ============================================================

def test_select():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute("SELECT 10")

            check_equal(
                cur.fetchone(),
                (10,),
                "SELECT",
            )


def test_expression():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT 10 + 20 * 2"
            )

            check_equal(
                cur.fetchone()[0],
                50,
                "Expression",
            )


def test_distinct():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT DISTINCT x
                FROM (
                    VALUES (1), (1), (2), (2), (3)
                ) AS t(x)
                ORDER BY x
                """
            )

            check_equal(
                cur.fetchall(),
                [(1,), (2,), (3,)],
                "DISTINCT",
            )


def test_case():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT CASE
                    WHEN 10 > 5 THEN 'yes'
                    ELSE 'no'
                END
                """
            )

            check_equal(
                cur.fetchone()[0],
                "yes",
                "CASE",
            )


def test_coalesce():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT COALESCE(NULL, 'fallback')"
            )

            check_equal(
                cur.fetchone()[0],
                "fallback",
                "COALESCE",
            )


def test_nullif():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT NULLIF(10, 10)"
            )

            check(
                cur.fetchone()[0] is None,
                "NULLIF",
            )


# ============================================================
# PARAMETERS
# ============================================================

def test_parameters():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    %s::integer,
                    %s::text,
                    %s::boolean,
                    %s::numeric
                """,
                (
                    10,
                    "hello",
                    True,
                    Decimal("12.50"),
                ),
            )

            row = cur.fetchone()

            check_equal(
                row[0],
                10,
                "integer parameter",
            )

            check_equal(
                row[1],
                "hello",
                "text parameter",
            )

            check_equal(
                row[2],
                True,
                "boolean parameter",
            )

            check_equal(
                row[3],
                Decimal("12.50"),
                "numeric parameter",
            )


# ============================================================
# DDL
# ============================================================

def table_name():
    return f"{TEST_SCHEMA}.users"


def create_test_table(conn):

    with conn.cursor() as cur:

        cur.execute(
            f"""
            CREATE TABLE IF NOT EXISTS
            {table_name()} (
                id BIGINT PRIMARY KEY,
                name TEXT NOT NULL,
                age INTEGER,
                salary NUMERIC(14,2),
                active BOOLEAN,
                email TEXT UNIQUE,
                birthday DATE,
                created_at TIMESTAMP,
                created_tz TIMESTAMPTZ,
                metadata JSONB
            )
            """
        )

    conn.commit()


def test_create_table():

    with connect() as conn:
        create_test_table(conn)


def test_create_index():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                CREATE INDEX IF NOT EXISTS
                idx_users_name
                ON {table_name()}(name)
                """
            )

        conn.commit()


def test_alter_table():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                ALTER TABLE {table_name()}
                ADD COLUMN IF NOT EXISTS
                notes TEXT
                """
            )

        conn.commit()


# ============================================================
# INSERT / SELECT / UPDATE / DELETE
# ============================================================

def test_insert():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                INSERT INTO {table_name()}
                    (id, name, age, salary, active)
                VALUES
                    (%s, %s, %s, %s, %s)
                """,
                (
                    1,
                    "Alice",
                    30,
                    Decimal("50000.50"),
                    True,
                ),
            )

            check_equal(
                cur.rowcount,
                1,
                "INSERT rowcount",
            )

        conn.commit()


def test_select_where():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT name
                FROM {table_name()}
                WHERE age >= %s
                """,
                (18,),
            )

            rows = cur.fetchall()

            check(
                isinstance(rows, list),
                "SELECT did not return list",
            )


def test_update():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                UPDATE {table_name()}
                SET age = age + 1
                WHERE id = %s
                """,
                (1,),
            )

            check_equal(
                cur.rowcount,
                1,
                "UPDATE rowcount",
            )

        conn.commit()


def test_delete():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                DELETE FROM {table_name()}
                WHERE id = %s
                """,
                (1,),
            )

            check_equal(
                cur.rowcount,
                1,
                "DELETE rowcount",
            )

        conn.commit()


# ============================================================
# BULK INSERT
# ============================================================

def test_executemany():

    with connect() as conn:

        with conn.cursor() as cur:

            rows = [
                (
                    i,
                    f"user-{i}",
                    20 + i % 40,
                    Decimal("1000.00"),
                    i % 2 == 0,
                )
                for i in range(10, 110)
            ]

            cur.executemany(
                f"""
                INSERT INTO {table_name()}
                    (id, name, age, salary, active)
                VALUES
                    (%s, %s, %s, %s, %s)
                """,
                rows,
            )

        conn.commit()


# ============================================================
# AGGREGATES
# ============================================================

def test_aggregates():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT
                    COUNT(*),
                    SUM(age),
                    AVG(age),
                    MIN(age),
                    MAX(age)
                FROM {table_name()}
                """
            )

            count, total, average, minimum, maximum = (
                cur.fetchone()
            )

            check(count > 0, "COUNT")
            check(total is not None, "SUM")
            check(average is not None, "AVG")
            check(minimum is not None, "MIN")
            check(maximum is not None, "MAX")


def test_group_by():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT active, COUNT(*)
                FROM {table_name()}
                GROUP BY active
                ORDER BY active
                """
            )

            rows = cur.fetchall()

            check(
                len(rows) >= 1,
                "GROUP BY",
            )


def test_having():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT active, COUNT(*)
                FROM {table_name()}
                GROUP BY active
                HAVING COUNT(*) > 0
                """
            )

            check(
                len(cur.fetchall()) > 0,
                "HAVING",
            )


# ============================================================
# JOINS / SUBQUERIES / CTE
# ============================================================

def test_inner_join():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT a.id
                FROM {table_name()} a
                INNER JOIN {table_name()} b
                    ON a.id = b.id
                """
            )

            rows = cur.fetchall()

            check(
                len(rows) > 0,
                "INNER JOIN",
            )


def test_left_join():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT a.id, b.id
                FROM {table_name()} a
                LEFT JOIN {table_name()} b
                    ON a.id = b.id
                """
            )

            rows = cur.fetchall()

            check(
                len(rows) > 0,
                "LEFT JOIN",
            )


def test_subquery():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT name
                FROM {table_name()}
                WHERE age > (
                    SELECT AVG(age)
                    FROM {table_name()}
                )
                """
            )

            cur.fetchall()


def test_cte():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                WITH numbers AS (
                    SELECT 1 AS n
                    UNION ALL
                    SELECT 2
                    UNION ALL
                    SELECT 3
                )
                SELECT SUM(n)
                FROM numbers
                """
            )

            check_equal(
                cur.fetchone()[0],
                6,
                "CTE",
            )


def test_union():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT 1
                UNION
                SELECT 2
                ORDER BY 1
                """
            )

            check_equal(
                cur.fetchall(),
                [(1,), (2,)],
                "UNION",
            )


# ============================================================
# TRANSACTIONS
# ============================================================

def test_commit():

    test_id = 1000001

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                INSERT INTO {table_name()}(id, name)
                VALUES (%s, %s)
                """,
                (
                    test_id,
                    "commit-test",
                ),
            )

        conn.commit()

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT name
                FROM {table_name()}
                WHERE id = %s
                """,
                (test_id,),
            )

            check_equal(
                cur.fetchone()[0],
                "commit-test",
                "COMMIT",
            )


def test_rollback():

    test_id = 1000002

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                INSERT INTO {table_name()}(id, name)
                VALUES (%s, %s)
                """,
                (
                    test_id,
                    "rollback-test",
                ),
            )

        conn.rollback()

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT id
                FROM {table_name()}
                WHERE id = %s
                """,
                (test_id,),
            )

            check(
                cur.fetchone() is None,
                "ROLLBACK did not remove row",
            )


def test_savepoint():

    first_id = 1000003
    second_id = 1000004

    with connect() as conn:

        with conn.transaction():

            with conn.cursor() as cur:

                cur.execute(
                    f"""
                    INSERT INTO {table_name()}(id, name)
                    VALUES (%s, %s)
                    """,
                    (
                        first_id,
                        "savepoint-one",
                    ),
                )

                with conn.transaction():

                    cur.execute(
                        f"""
                        INSERT INTO {table_name()}(id, name)
                        VALUES (%s, %s)
                        """,
                        (
                            second_id,
                            "savepoint-two",
                        ),
                    )

        conn.commit()


def test_failed_transaction_recovery():

    with connect() as conn:

        try:

            with conn.cursor() as cur:

                cur.execute(
                    "SELECT * FROM table_that_does_not_exist"
                )

        except Exception:
            pass

        conn.rollback()

        with conn.cursor() as cur:

            cur.execute("SELECT 1")

            check_equal(
                cur.fetchone(),
                (1,),
                "Transaction recovery",
            )


# ============================================================
# CONSTRAINTS / ERROR HANDLING
# ============================================================

def test_duplicate_primary_key():

    with connect() as conn:

        try:

            with conn.cursor() as cur:

                cur.execute(
                    f"""
                    INSERT INTO {table_name()}(id, name)
                    VALUES (%s, %s)
                    """,
                    (
                        10,
                        "duplicate",
                    ),
                )

                cur.execute(
                    f"""
                    INSERT INTO {table_name()}(id, name)
                    VALUES (%s, %s)
                    """,
                    (
                        10,
                        "duplicate-again",
                    ),
                )

        except errors.UniqueViolation:

            conn.rollback()
            return

        raise AssertionError(
            "Expected UniqueViolation"
        )


def test_not_null():

    with connect() as conn:

        try:

            with conn.cursor() as cur:

                cur.execute(
                    f"""
                    INSERT INTO {table_name()}(id, name)
                    VALUES (%s, %s)
                    """,
                    (
                        2000001,
                        None,
                    ),
                )

        except errors.NotNullViolation:

            conn.rollback()
            return

        raise AssertionError(
            "Expected NotNullViolation"
        )


def test_invalid_sql():

    with connect() as conn:

        try:

            with conn.cursor() as cur:

                cur.execute(
                    "THIS IS INVALID SQL"
                )

        except errors.SyntaxError:

            conn.rollback()
            return

        raise AssertionError(
            "Expected SyntaxError"
        )


def test_undefined_column():

    with connect() as conn:

        try:

            with conn.cursor() as cur:

                cur.execute(
                    f"""
                    SELECT column_that_does_not_exist
                    FROM {table_name()}
                    """
                )

        except errors.UndefinedColumn:

            conn.rollback()
            return

        raise AssertionError(
            "Expected UndefinedColumn"
        )


# ============================================================
# DATA TYPES
# ============================================================

def test_integer_types():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    1::smallint,
                    2::integer,
                    3::bigint
                """
            )

            row = cur.fetchone()

            check_equal(
                row[0],
                1,
                "smallint",
            )

            check_equal(
                row[1],
                2,
                "integer",
            )

            check_equal(
                row[2],
                3,
                "bigint",
            )


def test_numeric():

    value = Decimal("12345.6789")

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT %s::numeric",
                (value,),
            )

            check_equal(
                cur.fetchone()[0],
                value,
                "NUMERIC",
            )


def test_boolean():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT TRUE, FALSE"
            )

            true_value, false_value = cur.fetchone()

            check_equal(
                true_value,
                True,
                "TRUE",
            )

            check_equal(
                false_value,
                False,
                "FALSE",
            )


def test_date():

    value = date(2026, 1, 2)

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT %s::date",
                (value,),
            )

            check_equal(
                cur.fetchone()[0],
                value,
                "DATE",
            )


def test_timestamp():

    value = datetime(
        2026,
        1,
        2,
        12,
        30,
        0,
    )

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT %s::timestamp",
                (value,),
            )

            check_equal(
                cur.fetchone()[0],
                value,
                "TIMESTAMP",
            )


def test_timestamptz():

    value = datetime(
        2026,
        1,
        2,
        12,
        30,
        0,
        tzinfo=timezone.utc,
    )

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT %s::timestamptz",
                (value,),
            )

            result = cur.fetchone()[0]

            check(
                result is not None,
                "TIMESTAMPTZ returned NULL",
            )


# ============================================================
# STRING FUNCTIONS
# ============================================================

def test_string_functions():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    LENGTH('Plomid'),
                    LOWER('PLOMID'),
                    UPPER('plomid'),
                    SUBSTRING(
                        'Plomid'
                        FROM 1 FOR 3
                    )
                """
            )

            length, lower, upper, substring = (
                cur.fetchone()
            )

            check_equal(
                length,
                6,
                "LENGTH",
            )

            check_equal(
                lower,
                "plomid",
                "LOWER",
            )

            check_equal(
                upper,
                "PLOMID",
                "UPPER",
            )

            check_equal(
                substring,
                "Plo",
                "SUBSTRING",
            )


# ============================================================
# DATE FUNCTIONS
# ============================================================

def test_date_functions():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    EXTRACT(
                        YEAR
                        FROM DATE '2026-01-01'
                    ),
                    EXTRACT(
                        MONTH
                        FROM DATE '2026-01-01'
                    )
                """
            )

            year, month = cur.fetchone()

            check_equal(
                int(year),
                2026,
                "YEAR",
            )

            check_equal(
                int(month),
                1,
                "MONTH",
            )


# ============================================================
# JSON
# ============================================================

def test_json():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    '{"name":"Alice","age":30}'::json
                """
            )

            result = cur.fetchone()[0]

            check(
                result is not None,
                "JSON returned NULL",
            )


def test_jsonb():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    '{"name":"Alice","age":30}'::jsonb
                """
            )

            result = cur.fetchone()[0]

            check(
                result is not None,
                "JSONB returned NULL",
            )


# ============================================================
# ARRAYS
# ============================================================

def test_array():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT ARRAY[1,2,3]"
            )

            check_equal(
                cur.fetchone()[0],
                [1, 2, 3],
                "ARRAY",
            )


def test_array_parameter():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT %s::integer[]",
                ([1, 2, 3],),
            )

            check_equal(
                cur.fetchone()[0],
                [1, 2, 3],
                "Array parameter",
            )


# ============================================================
# CURSOR BEHAVIOR
# ============================================================

def test_fetchone():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute("SELECT 123")

            check_equal(
                cur.fetchone(),
                (123,),
                "fetchone",
            )


def test_fetchmany():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT generate_series(1, 10)
                """
            )

            rows = cur.fetchmany(3)

            check_equal(
                len(rows),
                3,
                "fetchmany",
            )


def test_description():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT
                    1 AS one,
                    'two' AS two
                """
            )

            description = cur.description

            check(
                description is not None,
                "cursor.description missing",
            )

            names = [
                column.name
                for column in description
            ]

            check_equal(
                names,
                ["one", "two"],
                "Column names",
            )


# ============================================================
# NULL SEMANTICS
# ============================================================

def test_null_semantics():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SELECT NULL = NULL"
            )

            check(
                cur.fetchone()[0] is None,
                "NULL = NULL should be NULL",
            )

            cur.execute(
                "SELECT NULL IS NULL"
            )

            check_equal(
                cur.fetchone()[0],
                True,
                "NULL IS NULL",
            )


# ============================================================
# CATALOG / INFORMATION_SCHEMA
# ============================================================

def test_information_schema():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT table_name
                FROM information_schema.tables
                WHERE table_schema = %s
                """,
                (TEST_SCHEMA,),
            )

            rows = cur.fetchall()

            names = [
                row[0]
                for row in rows
            ]

            check(
                "users" in names,
                "users missing from information_schema",
            )


def test_pg_type():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT oid, typname
                FROM pg_catalog.pg_type
                LIMIT 10
                """
            )

            rows = cur.fetchall()

            check(
                len(rows) > 0,
                "pg_type returned no rows",
            )


def test_pg_class():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT relname
                FROM pg_catalog.pg_class
                LIMIT 10
                """
            )

            rows = cur.fetchall()

            check(
                isinstance(rows, list),
                "pg_class invalid result",
            )


# ============================================================
# SESSION PARAMETERS
# ============================================================

def test_show_server_version():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SHOW server_version"
            )

            value = cur.fetchone()[0]

            check(
                value is not None,
                "server_version missing",
            )


def test_show_timezone():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SHOW TimeZone"
            )

            value = cur.fetchone()[0]

            check(
                value is not None,
                "TimeZone missing",
            )


# ============================================================
# EXPLAIN
# ============================================================

def test_explain():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                EXPLAIN
                SELECT *
                FROM {table_name()}
                """
            )

            rows = cur.fetchall()

            check(
                len(rows) > 0,
                "EXPLAIN returned no rows",
            )


# ============================================================
# CONCURRENCY
# ============================================================

def concurrency_worker(worker_id):

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                INSERT INTO {table_name()}
                    (id, name, age)
                VALUES (%s, %s, %s)
                """,
                (
                    3000000 + worker_id,
                    f"worker-{worker_id}",
                    20,
                ),
            )

        conn.commit()

    return worker_id


def test_concurrent_inserts():

    worker_count = WORKERS

    with ThreadPoolExecutor(
        max_workers=worker_count
    ) as executor:

        futures = [
            executor.submit(
                concurrency_worker,
                worker_id,
            )
            for worker_id in range(worker_count)
        ]

        for future in as_completed(futures):
            future.result()

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT COUNT(*)
                FROM {table_name()}
                WHERE id >= 3000000
                AND id < 3000000 + %s
                """,
                (worker_count,),
            )

            count = cur.fetchone()[0]

            check_equal(
                count,
                worker_count,
                "Concurrent insert count",
            )


# ============================================================
# CONCURRENT COUNTER
# ============================================================

def counter_setup():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                CREATE TABLE IF NOT EXISTS
                {TEST_SCHEMA}.counter (
                    id INTEGER PRIMARY KEY,
                    value INTEGER NOT NULL
                )
                """
            )

            cur.execute(
                f"""
                DELETE FROM
                {TEST_SCHEMA}.counter
                """
            )

            cur.execute(
                f"""
                INSERT INTO
                {TEST_SCHEMA}.counter(id, value)
                VALUES (1, 0)
                """
            )

        conn.commit()


def increment_counter(_):

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                UPDATE {TEST_SCHEMA}.counter
                SET value = value + 1
                WHERE id = 1
                """
            )

        conn.commit()


def test_concurrent_updates():

    counter_setup()

    operations = WORKERS * 10

    with ThreadPoolExecutor(
        max_workers=WORKERS
    ) as executor:

        futures = [
            executor.submit(
                increment_counter,
                i,
            )
            for i in range(operations)
        ]

        for future in as_completed(futures):
            future.result()

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT value
                FROM {TEST_SCHEMA}.counter
                WHERE id = 1
                """
            )

            value = cur.fetchone()[0]

            check_equal(
                value,
                operations,
                "Lost update detected",
            )


# ============================================================
# DEADLOCK / LOCKING
# ============================================================

def test_row_locking():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT id
                FROM {table_name()}
                WHERE id = 10
                FOR UPDATE
                """
            )

            cur.fetchall()

        conn.rollback()


# ============================================================
# LARGE DATA
# ============================================================

def test_large_insert():

    start = time.monotonic()

    with connect() as conn:

        with conn.cursor() as cur:

            rows = [
                (
                    5000000 + i,
                    f"stress-{i}",
                    i % 100,
                )
                for i in range(STRESS_ROWS)
            ]

            cur.executemany(
                f"""
                INSERT INTO {table_name()}
                    (id, name, age)
                VALUES (%s, %s, %s)
                """,
                rows,
            )

        conn.commit()

    elapsed = time.monotonic() - start

    print(
        f"       Inserted {STRESS_ROWS} rows "
        f"in {elapsed:.3f}s"
    )

    check(
        elapsed < 120,
        "Large insert took over 120 seconds",
    )


def test_large_select():

    start = time.monotonic()

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT COUNT(*)
                FROM {table_name()}
                WHERE id >= 5000000
                """
            )

            count = cur.fetchone()[0]

    elapsed = time.monotonic() - start

    print(
        f"       Selected {count} rows "
        f"in {elapsed:.3f}s"
    )

    check(
        count == STRESS_ROWS,
        "Large select row count",
    )


# ============================================================
# REPEATED QUERY
# ============================================================

def test_repeated_execution():

    with connect() as conn:

        with conn.cursor() as cur:

            for i in range(1000):

                cur.execute(
                    "SELECT %s::integer",
                    (i,),
                )

                value = cur.fetchone()[0]

                check_equal(
                    value,
                    i,
                    f"Iteration {i}",
                )


# ============================================================
# TRANSACTION ISOLATION
# ============================================================

def test_default_isolation():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "SHOW transaction_isolation"
            )

            isolation = cur.fetchone()[0]

            print(
                f"       transaction_isolation: "
                f"{isolation}"
            )

            check(
                isolation is not None,
                "Isolation level unavailable",
            )


# ============================================================
# LISTEN / NOTIFY
# ============================================================

def test_listen_notify():

    with connect() as conn:

        with conn.cursor() as cur:

            cur.execute(
                "LISTEN production_test_channel"
            )

        conn.commit()

        with conn.cursor() as cur:

            cur.execute(
                """
                NOTIFY production_test_channel,
                'hello'
                """
            )

        conn.commit()

        print(
            "       LISTEN / NOTIFY commands accepted"
        )


# ============================================================
# COPY
# ============================================================

def test_copy():

    with connect() as conn:

        with conn.cursor() as cur:

            with cur.copy(
                f"""
                COPY {table_name()}(id, name, age)
                FROM STDIN
                """
            ) as copy:

                copy.write_row(
                    (
                        9000001,
                        "copy-user",
                        42,
                    )
                )

        conn.commit()


# ============================================================
# TEST REGISTRY
# ============================================================

TESTS = [

    # Connectivity
    (
        "Connection",
        test_connection,
    ),
    (
        "Connection metadata",
        test_connection_metadata,
    ),

    # SQL
    (
        "SELECT",
        test_select,
    ),
    (
        "Expression evaluation",
        test_expression,
    ),
    (
        "DISTINCT",
        test_distinct,
    ),
    (
        "CASE",
        test_case,
    ),
    (
        "COALESCE",
        test_coalesce,
    ),
    (
        "NULLIF",
        test_nullif,
    ),

    # Parameters
    (
        "Parameters",
        test_parameters,
    ),

    # DDL
    (
        "CREATE TABLE",
        test_create_table,
    ),
    (
        "CREATE INDEX",
        test_create_index,
    ),
    (
        "ALTER TABLE",
        test_alter_table,
    ),

    # DML
    (
        "INSERT",
        test_insert,
    ),
    (
        "SELECT WHERE",
        test_select_where,
    ),
    (
        "UPDATE",
        test_update,
    ),
    (
        "DELETE",
        test_delete,
    ),
    (
        "executemany",
        test_executemany,
    ),

    # Aggregates
    (
        "Aggregates",
        test_aggregates,
    ),
    (
        "GROUP BY",
        test_group_by,
    ),
    (
        "HAVING",
        test_having,
    ),

    # Query features
    (
        "INNER JOIN",
        test_inner_join,
    ),
    (
        "LEFT JOIN",
        test_left_join,
    ),
    (
        "Subquery",
        test_subquery,
    ),
    (
        "CTE",
        test_cte,
    ),
    (
        "UNION",
        test_union,
    ),

    # Transactions
    (
        "COMMIT",
        test_commit,
    ),
    (
        "ROLLBACK",
        test_rollback,
    ),
    (
        "SAVEPOINT",
        test_savepoint,
    ),
    (
        "Failed transaction recovery",
        test_failed_transaction_recovery,
    ),

    # Errors
    (
        "Duplicate primary key",
        test_duplicate_primary_key,
    ),
    (
        "NOT NULL violation",
        test_not_null,
    ),
    (
        "Invalid SQL",
        test_invalid_sql,
    ),
    (
        "Undefined column",
        test_undefined_column,
    ),

    # Types
    (
        "Integer types",
        test_integer_types,
    ),
    (
        "NUMERIC",
        test_numeric,
    ),
    (
        "BOOLEAN",
        test_boolean,
    ),
    (
        "DATE",
        test_date,
    ),
    (
        "TIMESTAMP",
        test_timestamp,
    ),
    (
        "TIMESTAMPTZ",
        test_timestamptz,
    ),

    # Functions
    (
        "String functions",
        test_string_functions,
    ),
    (
        "Date functions",
        test_date_functions,
    ),

    # JSON
    (
        "JSON",
        test_json,
    ),
    (
        "JSONB",
        test_jsonb,
    ),

    # Arrays
    (
        "ARRAY",
        test_array,
    ),
    (
        "Array parameter",
        test_array_parameter,
    ),

    # Cursor
    (
        "fetchone",
        test_fetchone,
    ),
    (
        "fetchmany",
        test_fetchmany,
    ),
    (
        "cursor.description",
        test_description,
    ),

    # NULL
    (
        "NULL semantics",
        test_null_semantics,
    ),

    # PostgreSQL compatibility
    (
        "information_schema",
        test_information_schema,
    ),
    (
        "pg_type",
        test_pg_type,
    ),
    (
        "pg_class",
        test_pg_class,
    ),
    (
        "SHOW server_version",
        test_show_server_version,
    ),
    (
        "SHOW TimeZone",
        test_show_timezone,
    ),
    (
        "EXPLAIN",
        test_explain,
    ),

    # Concurrency
    (
        "Concurrent inserts",
        test_concurrent_inserts,
    ),
    (
        "Concurrent updates",
        test_concurrent_updates,
    ),
    (
        "Row locking",
        test_row_locking,
    ),

    # Large data
    (
        "Large insert",
        test_large_insert,
    ),
    (
        "Large select",
        test_large_select,
    ),

    # Repeated execution
    (
        "Repeated execution",
        test_repeated_execution,
    ),

    # Isolation
    (
        "Default transaction isolation",
        test_default_isolation,
    ),

    # Optional PostgreSQL features
    (
        "LISTEN / NOTIFY",
        test_listen_notify,
    ),
    (
        "COPY",
        test_copy,
    ),
]


# ============================================================
# SUMMARY
# ============================================================

def print_summary(script_start):

    total_process_time = (
        time.perf_counter() - script_start
    )

    title("FINAL RESULT")

    total = (
        PASSED
        + FAILED
        + SKIPPED
    )

    # --------------------------------------------------------
    # Result counts
    # --------------------------------------------------------

    print(
        f"PASSED:   {PASSED}"
    )

    print(
        f"FAILED:   {FAILED}"
    )

    print(
        f"SKIPPED:  {SKIPPED}"
    )

    print(
        f"TOTAL:    {total}"
    )

    # --------------------------------------------------------
    # Per-test timing
    # --------------------------------------------------------

    print()
    print("-" * 80)
    print("TEST EXECUTION TIMES")
    print("-" * 80)

    for result in RESULTS:

        duration = result.get(
            "elapsed",
            0.0,
        )

        status = result["status"]

        if status == "PASS":
            symbol = "✓"

        elif status == "FAIL":
            symbol = "✗"

        else:
            symbol = "⚠"

        print(
            f"{symbol} "
            f"{result['name']:<35} "
            f"{duration:>10.3f}s"
        )

    # --------------------------------------------------------
    # Timing statistics
    # --------------------------------------------------------

    test_execution_time = sum(
        result.get("elapsed", 0.0)
        for result in RESULTS
    )

    print()
    print("-" * 80)
    print("TIMING STATISTICS")
    print("-" * 80)

    print(
        f"Total process time:  "
        f"{total_process_time:.3f}s"
    )

    print(
        f"Total test time:     "
        f"{test_execution_time:.3f}s"
    )

    if RESULTS:

        average_test_time = (
            test_execution_time
            / len(RESULTS)
        )

        print(
            f"Average test time:   "
            f"{average_test_time:.3f}s"
        )

        slowest = max(
            RESULTS,
            key=lambda result:
                result.get("elapsed", 0.0),
        )

        fastest = min(
            RESULTS,
            key=lambda result:
                result.get("elapsed", 0.0),
        )

        print(
            f"Slowest test:        "
            f"{slowest['name']} "
            f"("
            f"{slowest.get('elapsed', 0.0):.3f}s"
            f")"
        )

        print(
            f"Fastest test:        "
            f"{fastest['name']} "
            f"("
            f"{fastest.get('elapsed', 0.0):.3f}s"
            f")"
        )

    # --------------------------------------------------------
    # Failed tests
    # --------------------------------------------------------

    print()

    if FAILED == 0 and SKIPPED == 0:

        print(
            "✓ ALL TESTS PASSED"
        )

        print(
            "✓ COMPATIBILITY TEST SUITE PASSED"
        )

    elif FAILED == 0:

        print(
            "⚠ NO FAILURES, "
            "BUT TESTS WERE SKIPPED"
        )

    else:

        print(
            "✗ TEST FAILURES DETECTED"
        )

    print()

    if FAILED:

        print("Failed tests:")

        for result in RESULTS:

            if result["status"] == "FAIL":

                print(
                    f"  ✗ {result['name']}: "
                    f"{result['error']} "
                    f"["
                    f"{result.get('elapsed', 0.0):.3f}s"
                    f"]"
                )


# ============================================================
# SECTION DETECTION
# ============================================================

def get_test_section(name):

    if name in {
        "Connection",
        "Connection metadata",
    }:
        return "CONNECTIVITY"

    elif name in {
        "SELECT",
        "Expression evaluation",
        "DISTINCT",
        "CASE",
        "COALESCE",
        "NULLIF",
    }:
        return "SQL"

    elif name in {
        "Parameters",
        "NULL parameters",
    }:
        return "PARAMETERS"

    elif name in {
        "CREATE TABLE",
        "CREATE INDEX",
        "ALTER TABLE",
    }:
        return "DDL"

    elif name in {
        "INSERT",
        "SELECT WHERE",
        "UPDATE",
        "DELETE",
        "executemany",
    }:
        return "DML"

    elif name in {
        "Aggregates",
        "GROUP BY",
        "HAVING",
    }:
        return "AGGREGATES"

    elif name in {
        "INNER JOIN",
        "LEFT JOIN",
        "Subquery",
        "CTE",
        "UNION",
    }:
        return "QUERY FEATURES"

    elif name in {
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT",
        "Failed transaction recovery",
    }:
        return "TRANSACTIONS"

    elif name in {
        "Duplicate primary key",
        "NOT NULL violation",
        "Invalid SQL",
        "Undefined column",
    }:
        return "ERROR HANDLING"

    elif name in {
        "Integer types",
        "NUMERIC",
        "BOOLEAN",
        "DATE",
        "TIMESTAMP",
        "TIMESTAMPTZ",
    }:
        return "DATA TYPES"

    elif name in {
        "String functions",
        "Date functions",
    }:
        return "FUNCTIONS"

    elif name in {
        "JSON",
        "JSONB",
        "ARRAY",
        "Array parameter",
    }:
        return "ADVANCED TYPES"

    elif name in {
        "fetchone",
        "fetchmany",
        "cursor.description",
    }:
        return "CURSORS"

    elif name == "NULL semantics":
        return "NULL SEMANTICS"

    elif name in {
        "information_schema",
        "pg_type",
        "pg_class",
        "SHOW server_version",
        "SHOW TimeZone",
        "EXPLAIN",
    }:
        return "POSTGRESQL COMPATIBILITY"

    elif name in {
        "Concurrent inserts",
        "Concurrent updates",
        "Row locking",
    }:
        return "CONCURRENCY"

    elif name in {
        "Large insert",
        "Large select",
    }:
        return "LARGE DATA"

    elif name == "Repeated execution":
        return "STABILITY"

    elif name == "Default transaction isolation":
        return "ISOLATION"

    return "OPTIONAL FEATURES"


# ============================================================
# RUNNER
# ============================================================

def main():

    script_start = time.perf_counter()

    title(
        "POSTGRESQL PRODUCTION "
        "COMPATIBILITY TEST SUITE"
    )

    print()

    print(
        f"DSN:     {DB_DSN}"
    )

    print(
        f"Schema:  {TEST_SCHEMA}"
    )

    print(
        f"Workers: {WORKERS}"
    )

    print(
        f"Rows:    {STRESS_ROWS}"
    )

    # --------------------------------------------------------
    # Connection
    # --------------------------------------------------------

    print()
    print(
        "Checking database connection..."
    )

    try:

        with connect() as conn:

            with conn.cursor() as cur:

                cur.execute("SELECT 1")

        print(
            "✓ Database connection successful"
        )

    except Exception as exc:

        print()
        print(
            "✗ DATABASE CONNECTION FAILED"
        )

        print(
            f"{type(exc).__name__}: {exc}"
        )

        sys.exit(2)

    # --------------------------------------------------------
    # Create isolated test schema
    # --------------------------------------------------------

    try:

        create_schema()

    except Exception as exc:

        print()
        print(
            "✗ Could not create test schema"
        )

        print(
            f"{type(exc).__name__}: {exc}"
        )

        sys.exit(2)

    # --------------------------------------------------------
    # Execute tests
    # --------------------------------------------------------

    current_section = None

    try:

        for name, function in TESTS:

            new_section = get_test_section(
                name
            )

            if new_section != current_section:

                section(
                    new_section
                )

                current_section = new_section

            run_test(
                name,
                function,
            )

    except KeyboardInterrupt:

        print()
        print(
            "Interrupted by user."
        )

    except Exception:

        print()
        print(
            "Unexpected test runner failure:"
        )

        traceback.print_exc()

    finally:

        print()
        print(
            "Cleaning test schema..."
        )

        cleanup_schema()

    # --------------------------------------------------------
    # Summary
    # --------------------------------------------------------

    print_summary(
        script_start
    )

    # --------------------------------------------------------
    # Exit code
    # --------------------------------------------------------

    if FAILED:
        sys.exit(1)

    if SKIPPED:
        sys.exit(3)

    sys.exit(0)


# ============================================================
# ENTRY POINT
# ============================================================

if __name__ == "__main__":
    main()
