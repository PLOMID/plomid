#!/usr/bin/env python3

"""
PLOMID PostgreSQL :: CAST TORTURE TEST

Purpose
-------
Extremely aggressive regression test for PostgreSQL's native:

    expression::TYPE

cast syntax.

IMPORTANT
---------
This test intentionally uses PostgreSQL double-colon casts.

There is NO CAST(expression AS TYPE) syntax anywhere in this file.

The goal is to detect parser failures around:

    ::
    nested casts
    casts + operators
    casts + functions
    casts + CASE
    casts + COALESCE
    casts + NULL
    casts + parameters
    casts + arrays
    casts + JSON / JSONB
    casts + timestamps
    casts + intervals
    casts + aggregates
    casts + joins
    casts + CTEs
    casts + subqueries
    casts + aliases
    casts in INSERT / UPDATE / SELECT
    invalid casts
    casts with whitespace/newlines
    casts in complex expressions
"""

import os
import sys
import time
import traceback
from datetime import date, datetime
from decimal import Decimal

import psycopg2


# ============================================================
# CONFIG
# ============================================================

HOST = os.getenv("PLOMID_HOST", "127.0.0.1")
PORT = int(os.getenv("PLOMID_PORT", "5432"))
USER = os.getenv("PLOMID_USER", "plomid")
PASSWORD = os.getenv("PLOMID_PASSWORD", "plomid")
DATABASE = os.getenv("PLOMID_DATABASE", "plomid")

SCHEMA = os.getenv(
    "PLOMID_CAST_SCHEMA",
    "plomid_cast_torture",
)

PASSED = 0
FAILED = 0
SKIPPED = 0
TESTS = 0

SUITE_START = time.perf_counter()


# ============================================================
# LOGGING
# ============================================================

def now():
    return datetime.now().strftime(
        "%Y-%m-%d %H:%M:%S.%f"
    )[:-3]


def log(level, message):
    print(
        f"{now()} [{level:<5}] {message}",
        flush=True,
    )


def section(title):
    print()
    print("=" * 100)
    print(f" {title}")
    print("=" * 100)


def elapsed_ms(start):
    return (
        time.perf_counter() - start
    ) * 1000


def compact_sql(sql):
    return " ".join(
        sql.strip().split()
    )


# ============================================================
# ASSERTIONS
# ============================================================

def fail(message):
    raise AssertionError(message)


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def eq(actual, expected, message=""):
    if actual != expected:
        raise AssertionError(
            f"{message} "
            f"expected={expected!r}, "
            f"actual={actual!r}"
        )


# ============================================================
# CONNECTION
# ============================================================

def connect():
    start = time.perf_counter()

    conn = psycopg2.connect(
        host=HOST,
        port=PORT,
        user=USER,
        password=PASSWORD,
        dbname=DATABASE,
        connect_timeout=10,
        application_name=(
            "PLOMID-POSTGRESQL-CAST-TORTURE"
        ),
    )

    conn.autocommit = True

    log(
        "INFO",
        f"connected in "
        f"{elapsed_ms(start):.2f} ms",
    )

    return conn


# ============================================================
# SQL EXECUTION
# ============================================================

def execute(
    conn,
    sql,
    params=None,
    fetch=False,
):
    cur = conn.cursor()

    start = time.perf_counter()

    try:
        log(
            "SQL",
            compact_sql(sql),
        )

        cur.execute(
            sql,
            params,
        )

        rows = None

        if fetch and cur.description:
            rows = cur.fetchall()

        row_count = (
            len(rows)
            if rows is not None
            else cur.rowcount
        )

        log(
            "QUERY",
            f"{elapsed_ms(start):.2f} ms "
            f"rows={row_count}",
        )

        return rows

    finally:
        cur.close()


def scalar(
    conn,
    sql,
    params=None,
):
    rows = execute(
        conn,
        sql,
        params,
        fetch=True,
    )

    if not rows:
        return None

    return rows[0][0]


# ============================================================
# TEST RUNNER
# ============================================================

def test(name, fn):
    global PASSED
    global FAILED
    global SKIPPED
    global TESTS

    TESTS += 1

    start = time.perf_counter()

    log(
        "TEST",
        name,
    )

    try:
        fn()

        PASSED += 1

        log(
            "PASS",
            f"{name} "
            f"[{elapsed_ms(start):.2f} ms]",
        )

    except NotImplementedError as exc:

        SKIPPED += 1

        log(
            "SKIP",
            f"{name}: {exc}",
        )

    except Exception as exc:

        FAILED += 1

        log(
            "FAIL",
            f"{name} "
            f"[{elapsed_ms(start):.2f} ms]",
        )

        log(
            "ERROR",
            f"{type(exc).__name__}: {exc}",
        )


# ============================================================
# EXPECTED CAST
# ============================================================

def expect_scalar(
    conn,
    name,
    sql,
    expected,
    params=None,
):
    start = time.perf_counter()

    try:
        actual = scalar(
            conn,
            sql,
            params,
        )

    except Exception as exc:

        raise AssertionError(
            f"""
CAST TEST FAILED

name:
    {name}

SQL:
    {sql}

params:
    {params!r}

error:
    {type(exc).__name__}: {exc}
"""
        ) from exc

    eq(
        actual,
        expected,
        f"{name}:",
    )

    log(
        "PASS",
        f"CAST {name} "
        f"result={actual!r} "
        f"[{elapsed_ms(start):.2f} ms]",
    )


# ============================================================
# BASIC SCALAR CASTS
# ============================================================

def test_basic_scalar_casts(conn):

    section(
        "LEVEL 1 - BASIC SCALAR CASTS"
    )

    tests = [

        (
            "integer",
            "SELECT 123::INTEGER",
            123,
        ),

        (
            "bigint",
            "SELECT 123::BIGINT",
            123,
        ),

        (
            "smallint",
            "SELECT 123::SMALLINT",
            123,
        ),

        (
            "numeric",
            "SELECT 123.45::NUMERIC",
            Decimal("123.45"),
        ),

        (
            "numeric precision",
            "SELECT 123.4567::NUMERIC(10,2)",
            Decimal("123.46"),
        ),

        (
            "real",
            "SELECT 123.5::REAL",
            123.5,
        ),

        (
            "double precision",
            "SELECT 123.5::DOUBLE PRECISION",
            123.5,
        ),

        (
            "text",
            "SELECT 123::TEXT",
            "123",
        ),

        (
            "varchar",
            "SELECT 123::VARCHAR",
            "123",
        ),

        (
            "char",
            "SELECT 123::CHAR",
            "1",
        ),

        (
            "boolean true",
            "SELECT 'true'::BOOLEAN",
            True,
        ),

        (
            "boolean false",
            "SELECT 'false'::BOOLEAN",
            False,
        ),

        (
            "date",
            "SELECT '2026-09-10'::DATE",
            date(2026, 9, 10),
        ),

        (
            "timestamp",
            "SELECT '2026-09-10 12:34:56'::TIMESTAMP",
            datetime(
                2026,
                9,
                10,
                12,
                34,
                56,
            ),
        ),

        (
            "time",
            "SELECT '12:34:56'::TIME",
            # psycopg2 returns datetime.time
            datetime.strptime(
                "12:34:56",
                "%H:%M:%S",
            ).time(),
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# INTEGER CAST CHAINS
# ============================================================

def test_integer_cast_chains(conn):

    section(
        "LEVEL 2 - NESTED INTEGER CAST CHAINS"
    )

    tests = [

        (
            "text -> integer",
            "SELECT '123'::INTEGER",
            123,
        ),

        (
            "text -> bigint",
            "SELECT '123'::BIGINT",
            123,
        ),

        (
            "text -> numeric -> integer",
            "SELECT '123.99'::NUMERIC::INTEGER",
            124,
        ),

        (
            "integer -> text -> integer",
            "SELECT 123::TEXT::INTEGER",
            123,
        ),

        (
            "integer -> bigint -> integer",
            "SELECT 123::INTEGER::BIGINT::INTEGER",
            123,
        ),

        (
            "integer -> numeric -> bigint",
            "SELECT 123::INTEGER::NUMERIC::BIGINT",
            123,
        ),

        (
            "integer arithmetic after cast",
            "SELECT '123'::INTEGER + 100",
            223,
        ),

        (
            "cast after arithmetic",
            "SELECT (100 + 23)::INTEGER",
            123,
        ),

        (
            "nested arithmetic cast",
            "SELECT ((100 + 20) * 3)::INTEGER",
            360,
        ),

        (
            "negative cast",
            "SELECT (-123)::INTEGER",
            -123,
        ),

        (
            "absolute value",
            "SELECT ABS('-123'::INTEGER)",
            123,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# NUMERIC TORTURE
# ============================================================

def test_numeric_casts(conn):

    section(
        "LEVEL 3 - NUMERIC CAST TORTURE"
    )

    tests = [

        (
            "decimal",
            "SELECT '999999.99'::NUMERIC",
            Decimal("999999.99"),
        ),

        (
            "precision scale",
            "SELECT '123.456'::NUMERIC(10,2)",
            Decimal("123.46"),
        ),

        (
            "negative numeric",
            "SELECT '-123.45'::NUMERIC",
            Decimal("-123.45"),
        ),

        (
            "numeric addition",
            "SELECT '10.50'::NUMERIC + '20.25'::NUMERIC",
            Decimal("30.75"),
        ),

        (
            "numeric multiplication",
            "SELECT '10.50'::NUMERIC * '2'::NUMERIC",
            Decimal("21.00"),
        ),

        (
            "numeric division",
            "SELECT '10'::NUMERIC / '4'::NUMERIC",
            Decimal("2.5"),
        ),

        (
            "numeric modulo",
            "SELECT '10'::NUMERIC % '3'::NUMERIC",
            Decimal("1"),
        ),

        (
            "integer to numeric",
            "SELECT 123::INTEGER::NUMERIC",
            Decimal("123"),
        ),

        (
            "text to numeric",
            "SELECT '123.456'::TEXT::NUMERIC",
            Decimal("123.456"),
        ),

        (
            "numeric to text",
            "SELECT 123.456::NUMERIC::TEXT",
            "123.456",
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# TEXT CAST TORTURE
# ============================================================

def test_text_casts(conn):

    section(
        "LEVEL 4 - TEXT CAST TORTURE"
    )

    tests = [

        (
            "integer -> text",
            "SELECT 123::TEXT",
            "123",
        ),

        (
            "numeric -> text",
            "SELECT 123.45::NUMERIC::TEXT",
            "123.45",
        ),

        (
            "boolean -> text",
            "SELECT TRUE::TEXT",
            "true",
        ),

        (
            "date -> text",
            "SELECT '2026-09-10'::DATE::TEXT",
            "2026-09-10",
        ),

        (
            "timestamp -> text",
            """
            SELECT
                '2026-09-10 12:34:56'::TIMESTAMP::TEXT
            """,
            "2026-09-10 12:34:56",
        ),

        (
            "text -> varchar",
            "SELECT 'hello'::TEXT::VARCHAR",
            "hello",
        ),

        (
            "varchar -> text",
            "SELECT 'hello'::VARCHAR::TEXT",
            "hello",
        ),

        (
            "concat cast",
            """
            SELECT
                ('user-' || 123::TEXT)::TEXT
            """,
            "user-123",
        ),

        (
            "uppercase after cast",
            """
            SELECT UPPER(
                123::TEXT
            )
            """,
            "123",
        ),

        (
            "length after cast",
            """
            SELECT LENGTH(
                12345::TEXT
            )
            """,
            5,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# BOOLEAN CASTS
# ============================================================

def test_boolean_casts(conn):

    section(
        "LEVEL 5 - BOOLEAN CAST TORTURE"
    )

    tests = [

        (
            "true text",
            "SELECT 'true'::BOOLEAN",
            True,
        ),

        (
            "false text",
            "SELECT 'false'::BOOLEAN",
            False,
        ),

        (
            "TRUE literal to text",
            "SELECT TRUE::TEXT",
            "true",
        ),

        (
            "FALSE literal to text",
            "SELECT FALSE::TEXT",
            "false",
        ),

        (
            "boolean CASE",
            """
            SELECT
                CASE
                    WHEN 'true'::BOOLEAN
                    THEN 100
                    ELSE 0
                END
            """,
            100,
        ),

        (
            "boolean NOT",
            """
            SELECT NOT ('false'::BOOLEAN)
            """,
            True,
        ),

        (
            "boolean equality",
            """
            SELECT
                'true'::BOOLEAN = TRUE
            """,
            True,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# DATE CASTS
# ============================================================

def test_date_casts(conn):

    section(
        "LEVEL 6 - DATE CAST TORTURE"
    )

    tests = [

        (
            "text -> date",
            "SELECT '2026-09-10'::DATE",
            date(2026, 9, 10),
        ),

        (
            "date -> text",
            "SELECT '2026-09-10'::DATE::TEXT",
            "2026-09-10",
        ),

        (
            "date + integer",
            "SELECT '2026-09-10'::DATE + 5",
            date(2026, 9, 15),
        ),

        (
            "date - integer",
            "SELECT '2026-09-10'::DATE - 5",
            date(2026, 9, 5),
        ),

        (
            "date interval",
            """
            SELECT
                '2026-09-10'::DATE
                + '5 days'::INTERVAL
            """,
            datetime(2026, 9, 15),
        ),

        (
            "date year extraction",
            """
            SELECT EXTRACT(
                YEAR FROM '2026-09-10'::DATE
            )
            """,
            Decimal("2026"),
        ),

        (
            "date month extraction",
            """
            SELECT EXTRACT(
                MONTH FROM '2026-09-10'::DATE
            )
            """,
            Decimal("9"),
        ),

        (
            "date trunc",
            """
            SELECT DATE_TRUNC(
                'month',
                '2026-09-10'::DATE
            )
            """,
            datetime(2026, 9, 1),
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# TIMESTAMP CASTS
# ============================================================

def test_timestamp_casts(conn):

    section(
        "LEVEL 7 - TIMESTAMP CAST TORTURE"
    )

    tests = [

        (
            "text -> timestamp",
            """
            SELECT
                '2026-09-10 12:34:56'::TIMESTAMP
            """,
            datetime(
                2026,
                9,
                10,
                12,
                34,
                56,
            ),
        ),

        (
            "timestamp -> date",
            """
            SELECT
                '2026-09-10 12:34:56'::TIMESTAMP::DATE
            """,
            date(2026, 9, 10),
        ),

        (
            "timestamp -> text",
            """
            SELECT
                '2026-09-10 12:34:56'::TIMESTAMP::TEXT
            """,
            "2026-09-10 12:34:56",
        ),

        (
            "timestamp + interval",
            """
            SELECT
                '2026-09-10 12:34:56'::TIMESTAMP
                + '1 day'::INTERVAL
            """,
            datetime(
                2026,
                9,
                11,
                12,
                34,
                56,
            ),
        ),

        (
            "timestamp - interval",
            """
            SELECT
                '2026-09-10 12:34:56'::TIMESTAMP
                - '1 hour'::INTERVAL
            """,
            datetime(
                2026,
                9,
                10,
                11,
                34,
                56,
            ),
        ),

        (
            "timestamp trunc",
            """
            SELECT DATE_TRUNC(
                'day',
                '2026-09-10 12:34:56'::TIMESTAMP
            )
            """,
            datetime(
                2026,
                9,
                10,
            ),
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# INTERVAL CASTS
# ============================================================

def test_interval_casts(conn):

    section(
        "LEVEL 8 - INTERVAL CAST TORTURE"
    )

    tests = [

        (
            "text -> interval",
            "SELECT '5 days'::INTERVAL",
            # PostgreSQL returns timedelta
            __import__("datetime").timedelta(days=5),
        ),

        (
            "timestamp + interval",
            """
            SELECT
                '2026-01-01'::DATE
                + '30 days'::INTERVAL
            """,
            datetime(
                2026,
                1,
                31,
            ),
        ),

        (
            "interval -> text",
            """
            SELECT
                '5 days'::INTERVAL::TEXT
            """,
            "5 days",
        ),

        (
            "interval multiplication",
            """
            SELECT
                '1 day'::INTERVAL * 5
            """,
            __import__("datetime").timedelta(days=5),
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# NULL CASTS
# ============================================================

def test_null_casts(conn):

    section(
        "LEVEL 9 - NULL CAST TORTURE"
    )

    tests = [

        (
            "NULL integer",
            "SELECT NULL::INTEGER",
            None,
        ),

        (
            "NULL bigint",
            "SELECT NULL::BIGINT",
            None,
        ),

        (
            "NULL numeric",
            "SELECT NULL::NUMERIC",
            None,
        ),

        (
            "NULL text",
            "SELECT NULL::TEXT",
            None,
        ),

        (
            "NULL date",
            "SELECT NULL::DATE",
            None,
        ),

        (
            "NULL timestamp",
            "SELECT NULL::TIMESTAMP",
            None,
        ),

        (
            "NULL boolean",
            "SELECT NULL::BOOLEAN",
            None,
        ),

        (
            "COALESCE NULL integer",
            """
            SELECT COALESCE(
                NULL::INTEGER,
                999
            )
            """,
            999,
        ),

        (
            "COALESCE NULL numeric",
            """
            SELECT COALESCE(
                NULL::NUMERIC,
                123.45::NUMERIC
            )
            """,
            Decimal("123.45"),
        ),

        (
            "CASE NULL",
            """
            SELECT
                CASE
                    WHEN NULL::BOOLEAN
                    THEN 1
                    ELSE 2
                END
            """,
            2,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# CASE / COALESCE
# ============================================================

def test_case_coalesce_casts(conn):

    section(
        "LEVEL 10 - CASE / COALESCE CASTS"
    )

    tests = [

        (
            "CASE integer cast",
            """
            SELECT CASE
                WHEN 100::INTEGER > 50
                THEN 'large'::TEXT
                ELSE 'small'::TEXT
            END
            """,
            "large",
        ),

        (
            "CASE numeric cast",
            """
            SELECT CASE
                WHEN '100.50'::NUMERIC > 50
                THEN 1::INTEGER
                ELSE 0::INTEGER
            END
            """,
            1,
        ),

        (
            "COALESCE integer",
            """
            SELECT COALESCE(
                NULL::INTEGER,
                '123'::INTEGER
            )
            """,
            123,
        ),

        (
            "COALESCE text",
            """
            SELECT COALESCE(
                NULL::TEXT,
                123::TEXT
            )
            """,
            "123",
        ),

        (
            "nested CASE",
            """
            SELECT CASE
                WHEN
                    (
                        CASE
                            WHEN '10'::INTEGER > 5
                            THEN 1::INTEGER
                            ELSE 0::INTEGER
                        END
                    ) = 1
                THEN 'YES'::TEXT
                ELSE 'NO'::TEXT
            END
            """,
            "YES",
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# ARRAY CASTS
# ============================================================

def test_array_casts(conn):

    section(
        "LEVEL 11 - ARRAY CAST TORTURE"
    )

    tests = [

        (
            "text array",
            "SELECT ARRAY['1','2','3']::INTEGER[]",
            [1, 2, 3],
        ),

        (
            "integer array",
            "SELECT ARRAY[1,2,3]::INTEGER[]",
            [1, 2, 3],
        ),

        (
            "text array to text",
            """
            SELECT
                ARRAY[1,2,3]::INTEGER[]::TEXT
            """,
            "{1,2,3}",
        ),

        (
            "array length",
            """
            SELECT ARRAY[1,2,3]::INTEGER[]::INTEGER[] 
                   IS NOT NULL
            """,
            True,
        ),

        (
            "array contains",
            """
            SELECT
                2 = ANY(
                    ARRAY[1,2,3]::INTEGER[]
                )
            """,
            True,
        ),

        (
            "array not contains",
            """
            SELECT
                9 = ANY(
                    ARRAY[1,2,3]::INTEGER[]
                )
            """,
            False,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            expected=expected,
            sql=sql,
        )


# ============================================================
# JSON / JSONB
# ============================================================

def test_json_casts(conn):

    section(
        "LEVEL 12 - JSON / JSONB CAST TORTURE"
    )

    tests = [

        (
            "text -> json",
            """
            SELECT
                '{"id":123,"name":"alice"}'::JSON
            """,
            '{"id":123,"name":"alice"}',
        ),

        (
            "text -> jsonb",
            """
            SELECT
                '{"id":123,"name":"alice"}'::JSONB
            """,
            '{"id":123,"name":"alice"}',
        ),

        (
            "json field -> integer",
            """
            SELECT
                ('{"id":123}'::JSON ->> 'id')::INTEGER
            """,
            123,
        ),

        (
            "jsonb field -> integer",
            """
            SELECT
                ('{"id":123}'::JSONB ->> 'id')::INTEGER
            """,
            123,
        ),

        (
            "json numeric field",
            """
            SELECT
                ('{"price":"123.45"}'::JSONB
                    ->> 'price')::NUMERIC
            """,
            Decimal("123.45"),
        ),

        (
            "json boolean field",
            """
            SELECT
                ('{"active":"true"}'::JSONB
                    ->> 'active')::BOOLEAN
            """,
            True,
        ),

        (
            "json nested field",
            """
            SELECT
                (
                    '{"user":{"id":"456"}}'::JSONB
                    -> 'user'
                    ->> 'id'
                )::INTEGER
            """,
            456,
        ),
    ]

    for name, sql, expected in tests:

        actual = scalar(conn, sql)

        if name in (
            "text -> json",
            "text -> jsonb",
        ):
            check(
                actual is not None,
                f"{name} returned NULL",
            )
        else:
            eq(
                actual,
                expected,
                name,
            )

        log(
            "PASS",
            f"JSON cast: {name}",
        )


# ============================================================
# CTE CASTS
# ============================================================

def test_cte_casts(conn):

    section(
        "LEVEL 13 - CTE CAST TORTURE"
    )

    sql = """
        WITH raw AS (
            SELECT
                '100'::TEXT AS raw_id,
                '250.75'::TEXT AS raw_amount,
                'true'::TEXT AS raw_active
        ),
        typed AS (
            SELECT
                raw_id::INTEGER AS id,
                raw_amount::NUMERIC AS amount,
                raw_active::BOOLEAN AS active
            FROM raw
        )
        SELECT
            id,
            amount,
            active
        FROM typed
    """

    rows = execute(
        conn,
        sql,
        fetch=True,
    )

    eq(
        rows,
        [
            (
                100,
                Decimal("250.75"),
                True,
            )
        ],
        "CTE cast chain",
    )


# ============================================================
# SUBQUERY CASTS
# ============================================================

def test_subquery_casts(conn):

    section(
        "LEVEL 14 - SUBQUERY CAST TORTURE"
    )

    tests = [

        (
            "scalar subquery integer",
            """
            SELECT (
                SELECT '123'::INTEGER
            )::BIGINT
            """,
            123,
        ),

        (
            "nested scalar cast",
            """
            SELECT (
                SELECT
                    (
                        '123.45'::NUMERIC
                    )::INTEGER
            )::TEXT
            """,
            "123",
        ),

        (
            "subquery arithmetic",
            """
            SELECT (
                SELECT '100'::INTEGER
            ) + (
                SELECT '23'::INTEGER
            )
            """,
            123,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# PARAMETERS + CASTS
# ============================================================

def test_parameter_casts(conn):

    section(
        "LEVEL 15 - PARAMETER + CAST TORTURE"
    )

    tests = [

        (
            "parameter integer",
            "SELECT %s::INTEGER",
            123,
            ("123",),
        ),

        (
            "parameter bigint",
            "SELECT %s::BIGINT",
            123,
            ("123",),
        ),

        (
            "parameter numeric",
            "SELECT %s::NUMERIC",
            Decimal("123.45"),
            ("123.45",),
        ),

        (
            "parameter boolean",
            "SELECT %s::BOOLEAN",
            True,
            ("true",),
        ),

        (
            "parameter date",
            "SELECT %s::DATE",
            date(2026, 9, 10),
            ("2026-09-10",),
        ),

        (
            "parameter timestamp",
            "SELECT %s::TIMESTAMP",
            datetime(
                2026,
                9,
                10,
                12,
                0,
                0,
            ),
            ("2026-09-10 12:00:00",),
        ),

        (
            "parameter arithmetic",
            "SELECT (%s::INTEGER * 10)::INTEGER",
            1230,
            ("123",),
        ),

        (
            "parameter nested",
            """
            SELECT
                (
                    (%s::TEXT)::INTEGER
                )::BIGINT
            """,
            123,
            ("123",),
        ),
    ]

    for name, sql, expected, params in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
            params,
        )


# ============================================================
# CREATE TEST TABLE
# ============================================================

def create_test_table(conn):

    section(
        "CREATE CAST TORTURE TABLE"
    )

    execute(
        conn,
        f"""
        CREATE SCHEMA IF NOT EXISTS {SCHEMA}
        """,
    )

    execute(
        conn,
        f"""
        DROP TABLE IF EXISTS
        {SCHEMA}.cast_values
        CASCADE
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.cast_values (
            id BIGINT PRIMARY KEY,
            raw_integer TEXT,
            raw_numeric TEXT,
            raw_boolean TEXT,
            raw_date TEXT,
            raw_timestamp TEXT,
            raw_json TEXT
        )
        """,
    )


# ============================================================
# INSERT CAST DATA
# ============================================================

def load_cast_data(conn):

    section(
        "INSERT CAST TORTURE DATA"
    )

    rows = [
        (
            1,
            "100",
            "100.25",
            "true",
            "2026-01-01",
            "2026-01-01 10:00:00",
            '{"id":100}',
        ),
        (
            2,
            "200",
            "200.50",
            "false",
            "2026-02-02",
            "2026-02-02 11:30:00",
            '{"id":200}',
        ),
        (
            3,
            "300",
            "300.75",
            "true",
            "2026-03-03",
            "2026-03-03 12:45:00",
            '{"id":300}',
        ),
        (
            4,
            "400",
            "400.00",
            "false",
            "2026-04-04",
            "2026-04-04 13:15:00",
            '{"id":400}',
        ),
        (
            5,
            "500",
            "500.50",
            "true",
            "2026-05-05",
            "2026-05-05 14:30:00",
            '{"id":500}',
        ),
    ]

    for row in rows:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.cast_values
            (
                id,
                raw_integer,
                raw_numeric,
                raw_boolean,
                raw_date,
                raw_timestamp,
                raw_json
            )
            VALUES (
                %s,
                %s,
                %s,
                %s,
                %s,
                %s,
                %s
            )
            """,
            row,
        )


# ============================================================
# CASTS INSIDE TABLE QUERIES
# ============================================================

def test_table_casts(conn):

    section(
        "LEVEL 16 - TABLE CAST TORTURE"
    )

    rows = execute(
        conn,
        f"""
        SELECT
            id::INTEGER,
            raw_integer::INTEGER,
            raw_numeric::NUMERIC,
            raw_boolean::BOOLEAN,
            raw_date::DATE,
            raw_timestamp::TIMESTAMP
        FROM {SCHEMA}.cast_values
        ORDER BY id
        """,
        fetch=True,
    )

    expected = [
        (
            1,
            100,
            Decimal("100.25"),
            True,
            date(2026, 1, 1),
            datetime(
                2026,
                1,
                1,
                10,
                0,
            ),
        ),
        (
            2,
            200,
            Decimal("200.50"),
            False,
            date(2026, 2, 2),
            datetime(
                2026,
                2,
                2,
                11,
                30,
            ),
        ),
        (
            3,
            300,
            Decimal("300.75"),
            True,
            date(2026, 3, 3),
            datetime(
                2026,
                3,
                3,
                12,
                45,
            ),
        ),
        (
            4,
            400,
            Decimal("400.00"),
            False,
            date(2026, 4, 4),
            datetime(
                2026,
                4,
                4,
                13,
                15,
            ),
        ),
        (
            5,
            500,
            Decimal("500.50"),
            True,
            date(2026, 5, 5),
            datetime(
                2026,
                5,
                5,
                14,
                30,
            ),
        ),
    ]

    eq(
        rows,
        expected,
        "table cast results",
    )


# ============================================================
# CAST + WHERE
# ============================================================

def test_where_casts(conn):

    section(
        "LEVEL 17 - WHERE CAST TORTURE"
    )

    tests = [

        (
            "integer WHERE",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_integer::INTEGER > 250
            """,
            3,
        ),

        (
            "numeric WHERE",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_numeric::NUMERIC >= 300.75::NUMERIC
            """,
            3,
        ),

        (
            "boolean WHERE",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_boolean::BOOLEAN = TRUE
            """,
            3,
        ),

               (
            "date WHERE",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_date::DATE >= '2026-03-03'::DATE
            """,
            3,
        ),

        (
            "timestamp WHERE",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_timestamp::TIMESTAMP
                  >= '2026-03-03 00:00:00'::TIMESTAMP
            """,
            3,
        ),

        (
            "text after integer cast",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_integer::INTEGER::TEXT = '300'
            """,
            1,
        ),

        (
            "numeric comparison",
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.cast_values
            WHERE raw_numeric::NUMERIC::INTEGER >= 300
            """,
            3,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# CAST + ORDER BY
# ============================================================

def test_order_by_casts(conn):

    section(
        "LEVEL 18 - ORDER BY CAST TORTURE"
    )

    tests = [

        (
            "order integer",
            f"""
            SELECT raw_integer::INTEGER
            FROM {SCHEMA}.cast_values
            ORDER BY raw_integer::INTEGER
            """,
            [100, 200, 300, 400, 500],
        ),

        (
            "order numeric",
            f"""
            SELECT raw_numeric::NUMERIC
            FROM {SCHEMA}.cast_values
            ORDER BY raw_numeric::NUMERIC DESC
            """,
            [
                Decimal("500.50"),
                Decimal("400.00"),
                Decimal("300.75"),
                Decimal("200.50"),
                Decimal("100.25"),
            ],
        ),

        (
            "order date",
            f"""
            SELECT raw_date::DATE
            FROM {SCHEMA}.cast_values
            ORDER BY raw_date::DATE
            """,
            [
                date(2026, 1, 1),
                date(2026, 2, 2),
                date(2026, 3, 3),
                date(2026, 4, 4),
                date(2026, 5, 5),
            ],
        ),
    ]

    for name, sql, expected in tests:

        rows = execute(
            conn,
            sql,
            fetch=True,
        )

        actual = [row[0] for row in rows]

        eq(
            actual,
            expected,
            name,
        )


# ============================================================
# CAST + GROUP BY
# ============================================================

def test_group_by_casts(conn):

    section(
        "LEVEL 19 - GROUP BY CAST TORTURE"
    )

    tests = [

        (
            "group integer",
            f"""
            SELECT
                raw_integer::INTEGER / 100 AS bucket,
                COUNT(*)::INTEGER
            FROM {SCHEMA}.cast_values
            GROUP BY raw_integer::INTEGER / 100
            ORDER BY bucket
            """,
            [
                (1, 1),
                (2, 1),
                (3, 1),
                (4, 1),
                (5, 1),
            ],
        ),

        (
            "group boolean",
            f"""
            SELECT
                raw_boolean::BOOLEAN,
                COUNT(*)::INTEGER
            FROM {SCHEMA}.cast_values
            GROUP BY raw_boolean::BOOLEAN
            ORDER BY raw_boolean::BOOLEAN
            """,
            [
                (False, 2),
                (True, 3),
            ],
        ),
    ]

    for name, sql, expected in tests:

        rows = execute(
            conn,
            sql,
            fetch=True,
        )

        eq(
            rows,
            expected,
            name,
        )


# ============================================================
# CAST + AGGREGATES
# ============================================================

def test_aggregate_casts(conn):

    section(
        "LEVEL 20 - AGGREGATE CAST TORTURE"
    )

    tests = [

        (
            "SUM integer",
            f"""
            SELECT SUM(
                raw_integer::INTEGER
            )::INTEGER
            FROM {SCHEMA}.cast_values
            """,
            1500,
        ),

        (
            "AVG integer",
            f"""
            SELECT AVG(
                raw_integer::INTEGER
            )::NUMERIC
            FROM {SCHEMA}.cast_values
            """,
            Decimal("300.0000000000000000"),
        ),

        (
            "SUM numeric",
            f"""
            SELECT SUM(
                raw_numeric::NUMERIC
            )
            FROM {SCHEMA}.cast_values
            """,
            Decimal("1502.00"),
        ),

        (
            "MIN integer",
            f"""
            SELECT MIN(
                raw_integer::INTEGER
            )
            FROM {SCHEMA}.cast_values
            """,
            100,
        ),

        (
            "MAX integer",
            f"""
            SELECT MAX(
                raw_integer::INTEGER
            )
            FROM {SCHEMA}.cast_values
            """,
            500,
        ),

        (
            "COUNT cast expression",
            f"""
            SELECT COUNT(
                raw_integer::INTEGER
            )::INTEGER
            FROM {SCHEMA}.cast_values
            """,
            5,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# CAST + JOIN
# ============================================================

def test_join_casts(conn):

    section(
        "LEVEL 21 - JOIN CAST TORTURE"
    )

    execute(
        conn,
        f"""
        DROP TABLE IF EXISTS
        {SCHEMA}.cast_join_values
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.cast_join_values (
            id TEXT,
            label TEXT
        )
        """,
    )

    rows = [
        ("1", "one"),
        ("2", "two"),
        ("3", "three"),
        ("4", "four"),
        ("5", "five"),
    ]

    for row in rows:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.cast_join_values
            (
                id,
                label
            )
            VALUES (
                %s,
                %s
            )
            """,
            row,
        )

    result = execute(
        conn,
        f"""
        SELECT
            c.id::INTEGER,
            j.label
        FROM {SCHEMA}.cast_values c
        JOIN {SCHEMA}.cast_join_values j
            ON c.id::INTEGER = j.id::INTEGER
        ORDER BY c.id::INTEGER
        """,
        fetch=True,
    )

    eq(
        result,
        [
            (1, "one"),
            (2, "two"),
            (3, "three"),
            (4, "four"),
            (5, "five"),
        ],
        "join cast results",
    )


# ============================================================
# INSERT ... SELECT CASTS
# ============================================================

def test_insert_select_casts(conn):

    section(
        "LEVEL 22 - INSERT SELECT CAST TORTURE"
    )

    execute(
        conn,
        f"""
        DROP TABLE IF EXISTS
        {SCHEMA}.cast_insert_target
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.cast_insert_target (
            id INTEGER,
            amount NUMERIC,
            active BOOLEAN
        )
        """,
    )

    execute(
        conn,
        f"""
        INSERT INTO {SCHEMA}.cast_insert_target
        (
            id,
            amount,
            active
        )
        SELECT
            raw_integer::INTEGER,
            raw_numeric::NUMERIC,
            raw_boolean::BOOLEAN
        FROM {SCHEMA}.cast_values
        ORDER BY id::INTEGER
        """,
    )

    rows = execute(
        conn,
        f"""
        SELECT
            id::INTEGER,
            amount::NUMERIC,
            active::BOOLEAN
        FROM {SCHEMA}.cast_insert_target
        ORDER BY id::INTEGER
        """,
        fetch=True,
    )

    eq(
        rows,
        [
            (1, Decimal("100.25"), True),
            (2, Decimal("200.50"), False),
            (3, Decimal("300.75"), True),
            (4, Decimal("400.00"), False),
            (5, Decimal("500.50"), True),
        ],
        "INSERT SELECT cast results",
    )


# ============================================================
# UPDATE CASTS
# ============================================================

def test_update_casts(conn):

    section(
        "LEVEL 23 - UPDATE CAST TORTURE"
    )

    execute(
        conn,
        f"""
        UPDATE {SCHEMA}.cast_values
        SET raw_integer =
            (raw_integer::INTEGER + 100)::TEXT
        """,
    )

    rows = execute(
        conn,
        f"""
        SELECT
            id::INTEGER,
            raw_integer::INTEGER
        FROM {SCHEMA}.cast_values
        ORDER BY id::INTEGER
        """,
        fetch=True,
    )

    eq(
        rows,
        [
            (1, 200),
            (2, 300),
            (3, 400),
            (4, 500),
            (5, 600),
        ],
        "UPDATE cast results",
    )


# ============================================================
# CAST + DISTINCT
# ============================================================

def test_distinct_casts(conn):

    section(
        "LEVEL 24 - DISTINCT CAST TORTURE"
    )

    rows = execute(
        conn,
        f"""
        SELECT DISTINCT
            raw_boolean::BOOLEAN
        FROM {SCHEMA}.cast_values
        ORDER BY raw_boolean::BOOLEAN
        """,
        fetch=True,
    )

    eq(
        rows,
        [
            (False,),
            (True,),
        ],
        "DISTINCT cast results",
    )


# ============================================================
# CAST + LIMIT / OFFSET
# ============================================================

def test_limit_offset_casts(conn):

    section(
        "LEVEL 25 - LIMIT / OFFSET CAST TORTURE"
    )

    rows = execute(
        conn,
        f"""
        SELECT
            raw_integer::INTEGER
        FROM {SCHEMA}.cast_values
        ORDER BY raw_integer::INTEGER
        LIMIT 2::INTEGER
        OFFSET 1::INTEGER
        """,
        fetch=True,
    )

    eq(
        rows,
        [
            (200,),
            (300,),
        ],
        "LIMIT OFFSET cast results",
    )


# ============================================================
# CAST + STRING FUNCTIONS
# ============================================================

def test_string_function_casts(conn):

    section(
        "LEVEL 26 - STRING FUNCTION CAST TORTURE"
    )

    tests = [

        (
            "substring cast",
            """
            SELECT SUBSTRING(
                123456::TEXT
                FROM 2
                FOR 3
            )
            """,
            "234",
        ),

        (
            "replace cast",
            """
            SELECT REPLACE(
                123123::TEXT,
                '1',
                '9'
            )
            """,
            "923923",
        ),

        (
            "position cast",
            """
            SELECT POSITION(
                '3' IN 12345::TEXT
            )
            """,
            3,
        ),

        (
            "left cast",
            """
            SELECT LEFT(
                123456::TEXT,
                3
            )
            """,
            "123",
        ),

        (
            "right cast",
            """
            SELECT RIGHT(
                123456::TEXT,
                3
            )
            """,
            "456",
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# CAST + MATHEMATICAL FUNCTIONS
# ============================================================

def test_math_function_casts(conn):

    section(
        "LEVEL 27 - MATHEMATICAL FUNCTION CAST TORTURE"
    )

    tests = [

        (
            "ABS",
            "SELECT ABS('-123'::INTEGER)",
            123,
        ),

        (
            "CEIL",
            "SELECT CEIL('123.45'::NUMERIC)",
            Decimal("124"),
        ),

        (
            "FLOOR",
            "SELECT FLOOR('123.99'::NUMERIC)",
            Decimal("123"),
        ),

        (
            "ROUND",
            "SELECT ROUND('123.456'::NUMERIC, 2)",
            Decimal("123.46"),
        ),

        (
            "POWER",
            "SELECT POWER(2::INTEGER, 3::INTEGER)",
            Decimal("8"),
        ),

        (
            "SQRT",
            "SELECT SQRT(144::NUMERIC)",
            Decimal("12"),
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# WHITESPACE / NEWLINE CASTS
# ============================================================

def test_whitespace_casts(conn):

    section(
        "LEVEL 28 - WHITESPACE / NEWLINE CAST TORTURE"
    )

    tests = [

        (
            "newline before cast",
            """
            SELECT
                123
                ::INTEGER
            """,
            123,
        ),

        (
            "newline after cast",
            """
            SELECT
                123::
                INTEGER
            """,
            123,
        ),

        (
            "multiple whitespace",
            """
            SELECT
                123
                ::
                INTEGER
            """,
            123,
        ),

        (
            "nested newline casts",
            """
            SELECT
                '123'
                ::
                TEXT
                ::
                INTEGER
            """,
            123,
        ),

        (
            "operator newline cast",
            """
            SELECT
                (
                    100
                    +
                    23
                )
                ::
                INTEGER
            """,
            123,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# DEEP NESTED CASTS
# ============================================================

def test_deep_nested_casts(conn):

    section(
        "LEVEL 29 - DEEP NESTED CAST TORTURE"
    )

    tests = [

        (
            "five level",
            """
            SELECT
                '123'
                ::TEXT
                ::VARCHAR
                ::NUMERIC
                ::INTEGER
                ::BIGINT
            """,
            123,
        ),

        (
            "numeric chain",
            """
            SELECT
                123
                ::INTEGER
                ::NUMERIC
                ::TEXT
                ::NUMERIC
                ::INTEGER
            """,
            123,
        ),

        (
            "date chain",
            """
            SELECT
                '2026-09-10'
                ::TEXT
                ::DATE
                ::TEXT
                ::DATE
            """,
            date(2026, 9, 10),
        ),

        (
            "boolean chain",
            """
            SELECT
                'true'
                ::TEXT
                ::BOOLEAN
                ::TEXT
                ::BOOLEAN
            """,
            True,
        ),
    ]

    for name, sql, expected in tests:

        expect_scalar(
            conn,
            name,
            sql,
            expected,
        )


# ============================================================
# INVALID CASTS
# ============================================================

def test_invalid_casts(conn):

    section(
        "LEVEL 30 - INVALID CAST TORTURE"

    )

    tests = [

        (
            "invalid integer",
            "SELECT 'not-an-integer'::INTEGER",
        ),

        (
            "invalid numeric",
            "SELECT 'not-a-number'::NUMERIC",
        ),

        (
            "invalid boolean",
            "SELECT 'not-a-boolean'::BOOLEAN",
        ),

        (
            "invalid date",
            "SELECT 'not-a-date'::DATE",
        ),

        (
            "invalid timestamp",
            "SELECT 'not-a-timestamp'::TIMESTAMP",
        ),

        (
            "invalid json",
            "SELECT '{invalid-json}'::JSON",
        ),

        (
            "invalid jsonb",
            "SELECT '{invalid-json}'::JSONB",
        ),
    ]

    for name, sql in tests:

        try:

            execute(
                conn,
                sql,
            )

        except psycopg2.Error:

            log(
                "PASS",
                f"expected failure: {name}",
            )

        else:

            raise AssertionError(
                f"Invalid cast unexpectedly succeeded: {name}"
            )


# ============================================================
# CLEANUP
# ============================================================

def cleanup(conn):

    section(
        "CLEANUP"
    )

    execute(
        conn,
        f"""
        DROP TABLE IF EXISTS
        {SCHEMA}.cast_insert_target
        CASCADE
        """,
    )

    execute(
        conn,
        f"""
        DROP TABLE IF EXISTS
        {SCHEMA}.cast_join_values
        CASCADE
        """,
    )

    execute(
        conn,
        f"""
        DROP TABLE IF EXISTS
        {SCHEMA}.cast_values
        CASCADE
        """,
    )

    execute(
        conn,
        f"""
        DROP SCHEMA IF EXISTS
        {SCHEMA}
        CASCADE
        """,
    )


# ============================================================
# MAIN
# ============================================================

def main():

    global SUITE_START

    SUITE_START = time.perf_counter()

    section(
        "PLOMID PostgreSQL :: CAST TORTURE TEST"
    )

    log(
        "INFO",
        f"host={HOST}",
    )

    log(
        "INFO",
        f"port={PORT}",
    )

    log(
        "INFO",
        f"user={USER}",
    )

    log(
        "INFO",
        f"database={DATABASE}",
    )

    log(
        "INFO",
        f"schema={SCHEMA}",
    )

    conn = None

    try:

        conn = connect()

        # ----------------------------------------------------
        # Pure expression tests
        # ----------------------------------------------------

        test(
            "basic scalar casts",
            lambda: test_basic_scalar_casts(conn),
        )

        test(
            "integer cast chains",
            lambda: test_integer_cast_chains(conn),
        )

        test(
            "numeric casts",
            lambda: test_numeric_casts(conn),
        )

        test(
            "text casts",
            lambda: test_text_casts(conn),
        )

        test(
            "boolean casts",
            lambda: test_boolean_casts(conn),
        )

        test(
            "date casts",
            lambda: test_date_casts(conn),
        )

        test(
            "timestamp casts",
            lambda: test_timestamp_casts(conn),
        )

        test(
            "interval casts",
            lambda: test_interval_casts(conn),
        )

        test(
            "NULL casts",
            lambda: test_null_casts(conn),
        )

        test(
            "CASE / COALESCE casts",
            lambda: test_case_coalesce_casts(conn),
        )

        test(
            "array casts",
            lambda: test_array_casts(conn),
        )

        test(
            "JSON / JSONB casts",
            lambda: test_json_casts(conn),
        )

        test(
            "CTE casts",
            lambda: test_cte_casts(conn),
        )

        test(
            "subquery casts",
            lambda: test_subquery_casts(conn),
        )

        test(
            "parameter casts",
            lambda: test_parameter_casts(conn),
        )

        # ----------------------------------------------------
        # Table tests
        # ----------------------------------------------------

        test(
            "create cast torture table",
            lambda: create_test_table(conn),
        )

        test(
            "load cast torture data",
            lambda: load_cast_data(conn),
        )

        test(
            "table casts",
            lambda: test_table_casts(conn),
        )

        test(
            "WHERE casts",
            lambda: test_where_casts(conn),
        )

        test(
            "ORDER BY casts",
            lambda: test_order_by_casts(conn),
        )

        test(
            "GROUP BY casts",
            lambda: test_group_by_casts(conn),
        )

        test(
            "aggregate casts",
            lambda: test_aggregate_casts(conn),
        )

        test(
            "JOIN casts",
            lambda: test_join_casts(conn),
        )

        test(
            "INSERT SELECT casts",
            lambda: test_insert_select_casts(conn),
        )

        test(
            "UPDATE casts",
            lambda: test_update_casts(conn),
        )

        test(
            "DISTINCT casts",
            lambda: test_distinct_casts(conn),
        )

        test(
            "LIMIT OFFSET casts",
            lambda: test_limit_offset_casts(conn),
        )

        test(
            "string function casts",
            lambda: test_string_function_casts(conn),
        )

        test(
            "math function casts",
            lambda: test_math_function_casts(conn),
        )

        test(
            "whitespace casts",
            lambda: test_whitespace_casts(conn),
        )

        test(
            "deep nested casts",
            lambda: test_deep_nested_casts(conn),
        )

        test(
            "invalid casts",
            lambda: test_invalid_casts(conn),
        )

    except KeyboardInterrupt:

        log(
            "WARN",
            "Interrupted by user",
        )

    except Exception as exc:

        log(
            "ERROR",
            f"Fatal error: "
            f"{type(exc).__name__}: {exc}",
        )

        traceback.print_exc()

    finally:

        if conn is not None:

            try:

                cleanup(conn)

            except Exception as exc:

                log(
                    "WARN",
                    f"cleanup failed: {exc}",
                )

            try:

                conn.close()

            except Exception:
                pass

    total_ms = elapsed_ms(
        SUITE_START
    )

    section(
        "FINAL RESULTS"
    )

    log(
        "INFO",
        f"TOTAL TESTS : {TESTS}",
    )

    log(
        "INFO",
        f"PASSED      : {PASSED}",
    )

    log(
        "INFO",
        f"FAILED      : {FAILED}",
    )

    log(
        "INFO",
        f"SKIPPED     : {SKIPPED}",
    )

    log(
        "INFO",
        f"TOTAL TIME  : {total_ms:.2f} ms",
    )

    if FAILED == 0:

        log(
            "PASS",
            "ALL CAST TESTS PASSED",
        )

        return 0

    log(
        "FAIL",
        "CAST TEST SUITE FAILED",
    )

    return 1


if __name__ == "__main__":
    sys.exit(main())
