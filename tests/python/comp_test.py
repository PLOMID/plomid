
#!/usr/bin/env python3

import sys
import traceback
from datetime import date, datetime, timezone
from decimal import Decimal

import psycopg
from psycopg import errors


# ============================================================
# CONFIGURATION
# ============================================================

# ------------------------------------------------------------
# Option 1: PostgreSQL connection string / DSN
# ------------------------------------------------------------

CONNECTION_STRING = (
    "postgresql://plomid:plomid@127.0.0.1:6000/plomid"
)


# ------------------------------------------------------------
# Option 2: Separate connection parameters
# ------------------------------------------------------------

DB_HOST = "127.0.0.1"
DB_PORT = 6000
DB_NAME = "plomid"
DB_USER = "plomid"
DB_PASSWORD = "plomid"


# ------------------------------------------------------------
# Test behavior
# ------------------------------------------------------------

# If True, a feature that Plomid does not implement yet is
# reported as UNSUPPORTED instead of FAIL.
ALLOW_UNSUPPORTED = True

TEST_SCHEMA = "plomid_compat"
TEST_TABLE = "plomid_compat.users"


# ============================================================
# TEST STATISTICS
# ============================================================

PASSED = 0
FAILED = 0
UNSUPPORTED = 0

# The connection used by all tests. It is a module-level global so the
# `test()` wrapper can roll back between tests; `main()` assigns it once the
# main connection is open.
conn = None


# ============================================================
# OUTPUT
# ============================================================

def title(text):
    print("\n" + "=" * 78)
    print(text)
    print("=" * 78)


def section(text):
    print("\n" + "-" * 78)
    print(text)
    print("-" * 78)


def test(name, function, unsupported=False):
    global PASSED, FAILED, UNSUPPORTED

    print(f"\n[TEST] {name}")

    try:
        conn.rollback()
    except Exception:
        pass

    try:
        function()

        print("       ✓ PASS")
        PASSED += 1

        return True

    except Exception as exc:

        try:
            conn.rollback()
        except Exception:
            pass

        if unsupported and ALLOW_UNSUPPORTED:
            print("       ⚠ UNSUPPORTED")
            print(f"       {type(exc).__name__}: {exc}")
            UNSUPPORTED += 1
            return False

        print("       ✗ FAIL")
        print(f"       {type(exc).__name__}: {exc}")

        FAILED += 1

        return False



def check(condition, message):
    if not condition:
        raise AssertionError(message)


# ============================================================
# CONNECTION TESTS
# ============================================================

def connect_using_dsn():
    return psycopg.connect(
        CONNECTION_STRING,
        connect_timeout=5,
    )


def connect_using_separate_credentials():
    return psycopg.connect(
        host=DB_HOST,
        port=DB_PORT,
        dbname=DB_NAME,
        user=DB_USER,
        password=DB_PASSWORD,
        connect_timeout=5,
    )


def test_dsn_connection():
    conn = connect_using_dsn()

    try:
        with conn.cursor() as cur:
            cur.execute("SELECT 1")
            result = cur.fetchone()

            check(
                result == (1,),
                f"Expected (1,), got {result}"
            )

    finally:
        conn.close()


def test_separate_credentials_connection():
    conn = connect_using_separate_credentials()

    try:
        with conn.cursor() as cur:
            cur.execute("SELECT 1")
            result = cur.fetchone()

            check(
                result == (1,),
                f"Expected (1,), got {result}"
            )

    finally:
        conn.close()


# ============================================================
# BASIC SQL
# ============================================================

def test_select(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT 1")
        check(cur.fetchone()[0] == 1, "SELECT 1 failed")


def test_expression(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT 10 + 20 * 2")
        check(cur.fetchone()[0] == 50, "Expression evaluation failed")


def test_alias(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT 123 AS value")

        row = cur.fetchone()

        check(row[0] == 123, "Alias query failed")


def test_distinct(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT DISTINCT value
            FROM (
                VALUES (1), (1), (2), (2), (3)
            ) AS x(value)
            ORDER BY value
        """)

        rows = cur.fetchall()

        check(
            rows == [(1,), (2,), (3,)],
            f"DISTINCT failed: {rows}"
        )


# ============================================================
# PARAMETERS / EXTENDED QUERY
# ============================================================

def test_integer_parameter(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::integer + %s::integer",
            (10, 20),
        )

        check(cur.fetchone()[0] == 30, "Parameter failed")


def test_string_parameter(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::text",
            ("Plomid",),
        )

        check(
            cur.fetchone()[0] == "Plomid",
            "Text parameter failed"
        )


def test_null_parameter(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s IS NULL",
            (None,),
        )

        check(
            cur.fetchone()[0] is True,
            "NULL parameter failed"
        )


def test_many_parameters(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT
                %s::integer,
                %s::text,
                %s::boolean,
                %s::numeric
        """, (
            10,
            "hello",
            True,
            Decimal("12.50"),
        ))

        row = cur.fetchone()

        check(row[0] == 10, "Integer parameter failed")
        check(row[1] == "hello", "Text parameter failed")
        check(row[2] is True, "Boolean parameter failed")
        check(row[3] == Decimal("12.50"), "Numeric parameter failed")


def test_repeated_execution(conn):
    with conn.cursor() as cur:
        for i in range(20):
            cur.execute(
                "SELECT %s::integer",
                (i,),
            )

            check(
                cur.fetchone()[0] == i,
                f"Iteration {i} failed"
            )


# ============================================================
# DDL
# ============================================================

def test_schema(conn):
    with conn.cursor() as cur:
        cur.execute(
            f"CREATE SCHEMA IF NOT EXISTS {TEST_SCHEMA}"
        )

    conn.commit()


def test_table(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            CREATE TABLE IF NOT EXISTS {TEST_TABLE} (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                age INTEGER,
                salary NUMERIC(12, 2),
                active BOOLEAN,
                email TEXT,
                created_at TIMESTAMP,
                created_tz TIMESTAMPTZ,
                birthday DATE,
                metadata JSONB
            )
        """)

    conn.commit()


def test_index(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            CREATE INDEX IF NOT EXISTS
            idx_users_name
            ON {TEST_TABLE}(name)
        """)

    conn.commit()


def test_alter_table(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            ALTER TABLE {TEST_TABLE}
            ADD COLUMN IF NOT EXISTS notes TEXT
        """)

    conn.commit()


# ============================================================
# INSERT
# ============================================================

def test_insert(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            INSERT INTO {TEST_TABLE}
                (id, name, age, salary, active)
            VALUES
                (%s, %s, %s, %s, %s)
        """, (
            1,
            "Alice",
            30,
            Decimal("50000.50"),
            True,
        ))

        check(cur.rowcount == 1, "INSERT rowcount incorrect")

    conn.commit()


def test_executemany(conn):
    with conn.cursor() as cur:
        cur.executemany(
            f"""
            INSERT INTO {TEST_TABLE}
                (id, name, age, active)
            VALUES (%s, %s, %s, %s)
            """,
            [
                (2, "Bob", 25, True),
                (3, "Charlie", 40, False),
                (4, "David", 35, True),
            ],
        )

    conn.commit()


# ============================================================
# SELECT
# ============================================================

def test_select_rows(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT id, name, age
            FROM {TEST_TABLE}
            ORDER BY id
        """)

        rows = cur.fetchall()

        print("       Rows:", rows)

        check(
            len(rows) >= 4,
            "Expected at least 4 rows"
        )


def test_where(conn):
    with conn.cursor() as cur:
        cur.execute(
            f"""
            SELECT name
            FROM {TEST_TABLE}
            WHERE age >= %s
            ORDER BY age
            """,
            (30,),
        )

        rows = cur.fetchall()

        check(
            len(rows) >= 2,
            "WHERE failed"
        )


def test_order_by(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT age
            FROM {TEST_TABLE}
            ORDER BY age DESC
        """)

        values = [row[0] for row in cur.fetchall()]

        check(
            values == sorted(values, reverse=True),
            "ORDER BY failed"
        )


def test_limit_offset(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT id
            FROM {TEST_TABLE}
            ORDER BY id
            LIMIT 2 OFFSET 1
        """)

        rows = cur.fetchall()

        check(
            len(rows) == 2,
            "LIMIT/OFFSET failed"
        )


# ============================================================
# UPDATE
# ============================================================

def test_update(conn):
    with conn.cursor() as cur:
        cur.execute(
            f"""
            UPDATE {TEST_TABLE}
            SET age = %s
            WHERE id = %s
            """,
            (31, 1),
        )

        check(
            cur.rowcount == 1,
            "UPDATE failed"
        )

    conn.commit()


def test_update_expression(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            UPDATE {TEST_TABLE}
            SET age = age + 1
            WHERE id = 1
        """)

        check(
            cur.rowcount == 1,
            "UPDATE expression failed"
        )

    conn.commit()


# ============================================================
# DELETE
# ============================================================

def test_delete(conn):
    with conn.cursor() as cur:
        cur.execute(
            f"""
            DELETE FROM {TEST_TABLE}
            WHERE id = %s
            """,
            (4,),
        )

        check(
            cur.rowcount == 1,
            "DELETE failed"
        )

    conn.commit()


# ============================================================
# AGGREGATES
# ============================================================

def test_count(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT COUNT(*)
            FROM {TEST_TABLE}
        """)

        count = cur.fetchone()[0]

        check(
            count >= 3,
            "COUNT failed"
        )


def test_sum(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT SUM(age)
            FROM {TEST_TABLE}
        """)

        check(
            cur.fetchone()[0] is not None,
            "SUM failed"
        )


def test_avg(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT AVG(age)
            FROM {TEST_TABLE}
        """)

        check(
            cur.fetchone()[0] is not None,
            "AVG failed"
        )


def test_min_max(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT MIN(age), MAX(age)
            FROM {TEST_TABLE}
        """)

        minimum, maximum = cur.fetchone()

        check(minimum is not None, "MIN failed")
        check(maximum is not None, "MAX failed")


# ============================================================
# GROUP / HAVING
# ============================================================

def test_group_by(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT active, COUNT(*)
            FROM {TEST_TABLE}
            GROUP BY active
            ORDER BY active
        """)

        rows = cur.fetchall()

        check(
            len(rows) >= 1,
            "GROUP BY failed"
        )


def test_having(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT active, COUNT(*)
            FROM {TEST_TABLE}
            GROUP BY active
            HAVING COUNT(*) >= 1
        """)

        rows = cur.fetchall()

        check(
            len(rows) >= 1,
            "HAVING failed"
        )


# ============================================================
# JOINS
# ============================================================

def test_inner_join(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT a.id, a.name
            FROM {TEST_TABLE} a
            INNER JOIN {TEST_TABLE} b
                ON a.id = b.id
            ORDER BY a.id
        """)

        rows = cur.fetchall()

        check(
            len(rows) >= 1,
            "INNER JOIN failed"
        )


def test_left_join(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT a.id, b.id
            FROM {TEST_TABLE} a
            LEFT JOIN {TEST_TABLE} b
                ON a.id = b.id
        """)

        rows = cur.fetchall()

        check(
            len(rows) >= 1,
            "LEFT JOIN failed"
        )


# ============================================================
# SUBQUERY / CTE / UNION
# ============================================================

def test_subquery(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT name
            FROM {TEST_TABLE}
            WHERE age > (
                SELECT AVG(age)
                FROM {TEST_TABLE}
            )
        """)

        cur.fetchall()


def test_cte(conn):
    with conn.cursor() as cur:
        cur.execute("""
            WITH numbers AS (
                SELECT 1 AS n
                UNION ALL
                SELECT 2
                UNION ALL
                SELECT 3
            )
            SELECT SUM(n)
            FROM numbers
        """)

        check(
            cur.fetchone()[0] == 6,
            "CTE failed"
        )


def test_union(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT 1
            UNION
            SELECT 2
            ORDER BY 1
        """)

        check(
            cur.fetchall() == [(1,), (2,)],
            "UNION failed"
        )


# ============================================================
# TRANSACTIONS
# ============================================================

def test_commit(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            INSERT INTO {TEST_TABLE}
                (id, name)
            VALUES (%s, %s)
        """, (100, "CommitTest"))

    conn.commit()

    with conn.cursor() as cur:
        cur.execute(
            f"""
            SELECT name
            FROM {TEST_TABLE}
            WHERE id = 100
            """
        )

        check(
            cur.fetchone()[0] == "CommitTest",
            "COMMIT failed"
        )


def test_rollback(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            INSERT INTO {TEST_TABLE}
                (id, name)
            VALUES (%s, %s)
        """, (101, "RollbackTest"))

    conn.rollback()

    with conn.cursor() as cur:
        cur.execute(
            f"""
            SELECT id
            FROM {TEST_TABLE}
            WHERE id = 101
            """
        )

        check(
            cur.fetchone() is None,
            "ROLLBACK failed"
        )


def test_savepoint(conn):
    with conn.transaction():
        with conn.cursor() as cur:
            cur.execute(f"""
                INSERT INTO {TEST_TABLE}
                    (id, name)
                VALUES (%s, %s)
            """, (102, "SavepointTest"))

    conn.commit()


# ============================================================
# NULL
# ============================================================

def test_null(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            INSERT INTO {TEST_TABLE}
                (id, name, age, active)
            VALUES (%s, %s, %s, %s)
        """, (103, "NullTest", None, None))

    conn.commit()

    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT age, active
            FROM {TEST_TABLE}
            WHERE id = 103
        """)

        age, active = cur.fetchone()

        check(age is None, "NULL integer failed")
        check(active is None, "NULL boolean failed")


# ============================================================
# DATA TYPES
# ============================================================

def test_integer_types(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT
                1::smallint,
                2::integer,
                3::bigint
        """)

        row = cur.fetchone()

        check(row[0] == 1, "smallint failed")
        check(row[1] == 2, "integer failed")
        check(row[2] == 3, "bigint failed")


def test_numeric(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::numeric",
            (Decimal("12345.6789"),)
        )

        result = cur.fetchone()[0]

        check(
            result == Decimal("12345.6789"),
            f"NUMERIC failed: {result}"
        )


def test_float(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::double precision",
            (3.14159,)
        )

        result = cur.fetchone()[0]

        check(
            abs(result - 3.14159) < 0.00001,
            "DOUBLE PRECISION failed"
        )


def test_boolean(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT TRUE, FALSE"
        )

        true_value, false_value = cur.fetchone()

        check(true_value is True, "TRUE failed")
        check(false_value is False, "FALSE failed")


def test_date(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::date",
            (date(2026, 1, 2),)
        )

        result = cur.fetchone()[0]

        check(
            result == date(2026, 1, 2),
            "DATE failed"
        )


def test_timestamp(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::timestamp",
            (datetime(2026, 1, 2, 12, 30, 0),)
        )

        result = cur.fetchone()[0]

        print("       Timestamp:", result)


def test_timestamptz(conn):
    with conn.cursor() as cur:
        value = datetime(
            2026,
            1,
            2,
            12,
            30,
            0,
            tzinfo=timezone.utc,
        )

        cur.execute(
            "SELECT %s::timestamptz",
            (value,)
        )

        result = cur.fetchone()[0]

        print("       TimestampTZ:", result)


# ============================================================
# STRING FUNCTIONS
# ============================================================

def test_string_functions(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT
                LENGTH('Plomid'),
                LOWER('PLOMID'),
                UPPER('plomid'),
                SUBSTRING('Plomid' FROM 1 FOR 3)
        """)

        row = cur.fetchone()

        check(row[0] == 6, "LENGTH failed")
        check(row[1] == "plomid", "LOWER failed")
        check(row[2] == "PLOMID", "UPPER failed")
        check(row[3] == "Plo", "SUBSTRING failed")


# ============================================================
# DATE FUNCTIONS
# ============================================================

def test_date_functions(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT
                EXTRACT(YEAR FROM DATE '2026-01-01'),
                EXTRACT(MONTH FROM DATE '2026-01-01')
        """)

        year, month = cur.fetchone()

        check(int(year) == 2026, "YEAR extraction failed")
        check(int(month) == 1, "MONTH extraction failed")


# ============================================================
# JSON
# ============================================================

def test_json(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT '{"name":"Alice","age":30}'::json
        """)

        result = cur.fetchone()[0]

        print("       JSON:", result)


def test_jsonb(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT '{"name":"Alice","age":30}'::jsonb
        """)

        result = cur.fetchone()[0]

        print("       JSONB:", result)


# ============================================================
# ARRAYS
# ============================================================

def test_array(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT ARRAY[1, 2, 3]
        """)

        result = cur.fetchone()[0]

        print("       Array:", result)

        check(
            result == [1, 2, 3],
            f"Array failed: {result}"
        )


def test_array_parameter(conn):
    with conn.cursor() as cur:
        cur.execute(
            "SELECT %s::integer[]",
            ([1, 2, 3],)
        )

        result = cur.fetchone()[0]

        check(
            result == [1, 2, 3],
            "Array parameter failed"
        )


# ============================================================
# CASE / COALESCE / NULLIF
# ============================================================

def test_case(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT CASE
                WHEN 10 > 5 THEN 'yes'
                ELSE 'no'
            END
        """)

        check(
            cur.fetchone()[0] == "yes",
            "CASE failed"
        )


def test_coalesce(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT COALESCE(NULL, 'fallback')
        """)

        check(
            cur.fetchone()[0] == "fallback",
            "COALESCE failed"
        )


def test_nullif(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT NULLIF(10, 10)
        """)

        check(
            cur.fetchone()[0] is None,
            "NULLIF failed"
        )


# ============================================================
# EXPLAIN
# ============================================================

def test_explain(conn):
    with conn.cursor() as cur:
        cur.execute(
            f"EXPLAIN SELECT * FROM {TEST_TABLE}"
        )

        rows = cur.fetchall()

        check(
            len(rows) > 0,
            "EXPLAIN returned no rows"
        )


# ============================================================
# POSTGRES CATALOG
# ============================================================

def test_pg_type(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT oid, typname
            FROM pg_catalog.pg_type
            LIMIT 10
        """)

        rows = cur.fetchall()

        check(
            len(rows) > 0,
            "pg_type is empty"
        )


def test_pg_class(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT relname
            FROM pg_catalog.pg_class
            LIMIT 10
        """)

        rows = cur.fetchall()

        check(
            isinstance(rows, list),
            "pg_class failed"
        )


def test_pg_namespace(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT nspname
            FROM pg_catalog.pg_namespace
            LIMIT 10
        """)

        rows = cur.fetchall()

        check(
            isinstance(rows, list),
            "pg_namespace failed"
        )


def test_information_schema(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT table_name
            FROM information_schema.tables
            WHERE table_name = %s
        """, ("users",))

        rows = cur.fetchall()

        print("       Tables:", rows)


def test_current_database(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT current_database()")

        result = cur.fetchone()[0]

        print("       Database:", result)


def test_current_schema(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT current_schema()")

        result = cur.fetchone()[0]

        print("       Schema:", result)


def test_version(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT version()")

        result = cur.fetchone()[0]

        print("       Version:", result)


# ============================================================
# ERROR HANDLING
# ============================================================

def test_duplicate_primary_key(conn):
    try:
        with conn.cursor() as cur:
            cur.execute(
                f"""
                INSERT INTO {TEST_TABLE}
                    (id, name)
                VALUES (%s, %s)
                """,
                (1, "Duplicate"),
            )

    except errors.UniqueViolation:
        conn.rollback()
        return

    conn.rollback()

    raise AssertionError(
        "Expected PostgreSQL UniqueViolation"
    )


def test_not_null(conn):
    try:
        with conn.cursor() as cur:
            cur.execute(
                f"""
                INSERT INTO {TEST_TABLE}
                    (id, name)
                VALUES (%s, %s)
                """,
                (500, None),
            )

    except errors.NotNullViolation:
        conn.rollback()
        return

    conn.rollback()

    raise AssertionError(
        "Expected PostgreSQL NotNullViolation"
    )


def test_invalid_sql(conn):
    try:
        with conn.cursor() as cur:
            cur.execute(
                "THIS IS NOT VALID SQL"
            )

    except errors.SyntaxError:
        conn.rollback()
        return

    conn.rollback()

    raise AssertionError(
        "Expected PostgreSQL SyntaxError"
    )


def test_undefined_table(conn):
    try:
        with conn.cursor() as cur:
            cur.execute("""
                SELECT *
                FROM definitely_missing_plomid_table
            """)

    except errors.UndefinedTable:
        conn.rollback()
        return

    conn.rollback()

    raise AssertionError(
        "Expected PostgreSQL UndefinedTable"
    )


def test_undefined_column(conn):
    try:
        with conn.cursor() as cur:
            cur.execute(f"""
                SELECT definitely_missing_column
                FROM {TEST_TABLE}
            """)

    except errors.UndefinedColumn:
        conn.rollback()
        return

    conn.rollback()

    raise AssertionError(
        "Expected PostgreSQL UndefinedColumn"
    )


# ============================================================
# COPY
# ============================================================

def test_copy(conn):
    with cur_context(conn) as cur:

        with cur.copy(
            f"""
            COPY {TEST_TABLE}(id, name, age)
            FROM STDIN
            """
        ) as copy:

            copy.write_row(
                (600, "CopyUser", 20)
            )

    conn.commit()


# ============================================================
# CURSOR / FETCH
# ============================================================

def test_fetchmany(conn):
    with conn.cursor() as cur:
        cur.execute(f"""
            SELECT id, name
            FROM {TEST_TABLE}
            ORDER BY id
        """)

        rows = cur.fetchmany(2)

        check(
            len(rows) <= 2,
            "fetchmany failed"
        )


def test_fetchone(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT 123
        """)

        row = cur.fetchone()

        check(
            row == (123,),
            "fetchone failed"
        )


def test_description(conn):
    with conn.cursor() as cur:
        cur.execute("""
            SELECT 1 AS one, 'two' AS two
        """)

        description = cur.description

        check(
            description is not None,
            "Cursor description missing"
        )

        print(
            "       Columns:",
            [column.name for column in description]
        )


# ============================================================
# SESSION PARAMETERS
# ============================================================

def test_show_server_version(conn):
    with conn.cursor() as cur:
        cur.execute("SHOW server_version")

        print(
            "       server_version:",
            cur.fetchone()[0]
        )


def test_show_client_encoding(conn):
    with conn.cursor() as cur:
        cur.execute("SHOW client_encoding")

        print(
            "       client_encoding:",
            cur.fetchone()[0]
        )


def test_show_timezone(conn):
    with conn.cursor() as cur:
        cur.execute("SHOW TimeZone")

        print(
            "       TimeZone:",
            cur.fetchone()[0]
        )


# ============================================================
# TRANSACTION ERROR STATE
# ============================================================

def test_failed_transaction_recovery(conn):
    try:
        with conn.cursor() as cur:
            cur.execute(
                "SELECT * FROM missing_table_for_transaction_test"
            )

    except Exception:
        pass

    # PostgreSQL requires ROLLBACK after an error
    # before another command can execute.
    conn.rollback()

    with conn.cursor() as cur:
        cur.execute("SELECT 1")

        check(
            cur.fetchone()[0] == 1,
            "Failed transaction recovery failed"
        )


# ============================================================
# OPTIONAL FEATURES
# ============================================================

def test_listen_notify(conn):
    with conn.cursor() as cur:
        cur.execute(
            "LISTEN plomid_test_channel"
        )

    conn.commit()

    with conn.cursor() as cur:
        cur.execute(
            "NOTIFY plomid_test_channel, 'hello'"
        )

    conn.commit()

    print(
        "       LISTEN/NOTIFY command accepted"
    )


def test_copy_feature(conn):
    with conn.cursor() as cur:
        with cur.copy(
            f"""
            COPY {TEST_TABLE}(id, name, age)
            FROM STDIN
            """
        ) as copy:
            copy.write_row(
                (700, "CopyTest", 50)
            )

    conn.commit()


# ============================================================
# CONTEXT MANAGER
# ============================================================

class cur_context:
    def __init__(self, conn):
        self.conn = conn
        self.cur = None

    def __enter__(self):
        self.cur = self.conn.cursor()
        return self.cur

    def __exit__(self, exc_type, exc_value, traceback_value):
        self.cur.close()


# ============================================================
# CLEANUP
# ============================================================

def cleanup(conn):
    print("\nCleaning test objects...")

    try:
        with conn.cursor() as cur:

            cur.execute(
                f"DROP SCHEMA IF EXISTS "
                f"{TEST_SCHEMA} CASCADE"
            )

        conn.commit()

        print("✓ Cleanup complete")

    except Exception as exc:
        conn.rollback()

        print(
            "⚠ Cleanup failed:",
            type(exc).__name__,
            exc
        )


# ============================================================
# RUN ALL TESTS
# ============================================================

def run_suite(conn):
    section("BASIC SQL")

    test("SELECT 1", lambda: test_select(conn))
    test("Expression evaluation", lambda: test_expression(conn))
    test("Column aliases", lambda: test_alias(conn))
    test("DISTINCT", lambda: test_distinct(conn))


    section("PARAMETERS / EXTENDED QUERY")

    test(
        "Integer parameters",
        lambda: test_integer_parameter(conn)
    )

    test(
        "String parameters",
        lambda: test_string_parameter(conn)
    )

    test(
        "NULL parameters",
        lambda: test_null_parameter(conn)
    )

    test(
        "Multiple parameter types",
        lambda: test_many_parameters(conn)
    )

    test(
        "Repeated parameterized execution",
        lambda: test_repeated_execution(conn)
    )


    section("DDL")

    test(
        "CREATE SCHEMA",
        lambda: test_schema(conn)
    )

    test(
        "CREATE TABLE",
        lambda: test_table(conn)
    )

    test(
        "CREATE INDEX",
        lambda: test_index(conn)
    )

    test(
        "ALTER TABLE",
        lambda: test_alter_table(conn)
    )


    section("DML")

    test(
        "INSERT",
        lambda: test_insert(conn)
    )

    test(
        "executemany INSERT",
        lambda: test_executemany(conn)
    )

    test(
        "SELECT rows",
        lambda: test_select_rows(conn)
    )

    test(
        "WHERE",
        lambda: test_where(conn)
    )

    test(
        "ORDER BY",
        lambda: test_order_by(conn)
    )

    test(
        "LIMIT/OFFSET",
        lambda: test_limit_offset(conn)
    )

    test(
        "UPDATE",
        lambda: test_update(conn)
    )

    test(
        "UPDATE expression",
        lambda: test_update_expression(conn)
    )

    test(
        "DELETE",
        lambda: test_delete(conn)
    )


    section("AGGREGATES")

    test(
        "COUNT",
        lambda: test_count(conn)
    )

    test(
        "SUM",
        lambda: test_sum(conn)
    )

    test(
        "AVG",
        lambda: test_avg(conn)
    )

    test(
        "MIN/MAX",
        lambda: test_min_max(conn)
    )

    test(
        "GROUP BY",
        lambda: test_group_by(conn)
    )

    test(
        "HAVING",
        lambda: test_having(conn)
    )


    section("JOINS")

    test(
        "INNER JOIN",
        lambda: test_inner_join(conn)
    )

    test(
        "LEFT JOIN",
        lambda: test_left_join(conn)
    )


    section("SUBQUERIES / CTE / UNION")

    test(
        "Subquery",
        lambda: test_subquery(conn)
    )

    test(
        "CTE",
        lambda: test_cte(conn)
    )

    test(
        "UNION",
        lambda: test_union(conn)
    )


    section("TRANSACTIONS")

    test(
        "COMMIT",
        lambda: test_commit(conn)
    )

    test(
        "ROLLBACK",
        lambda: test_rollback(conn)
    )

    test(
        "SAVEPOINT / transaction nesting",
        lambda: test_savepoint(conn),
        unsupported=True,
    )


    section("NULL")

    test(
        "NULL handling",
        lambda: test_null(conn)
    )


    section("DATA TYPES")

    test(
        "Integer types",
        lambda: test_integer_types(conn)
    )

    test(
        "NUMERIC",
        lambda: test_numeric(conn)
    )

    test(
        "DOUBLE PRECISION",
        lambda: test_float(conn)
    )

    test(
        "BOOLEAN",
        lambda: test_boolean(conn)
    )

    test(
        "DATE",
        lambda: test_date(conn)
    )

    test(
        "TIMESTAMP",
        lambda: test_timestamp(conn)
    )

    test(
        "TIMESTAMPTZ",
        lambda: test_timestamptz(conn),
        unsupported=True,
    )


    section("STRING / NULL FUNCTIONS")

    test(
        "String functions",
        lambda: test_string_functions(conn)
    )

    test(
        "CASE",
        lambda: test_case(conn)
    )

    test(
        "COALESCE",
        lambda: test_coalesce(conn)
    )

    test(
        "NULLIF",
        lambda: test_nullif(conn)
    )


    section("DATE FUNCTIONS")

    test(
        "DATE extraction",
        lambda: test_date_functions(conn)
    )


    section("JSON")

    test(
        "JSON",
        lambda: test_json(conn),
        unsupported=True,
    )

    test(
        "JSONB",
        lambda: test_jsonb(conn),
        unsupported=True,
    )


    section("ARRAYS")

    test(
        "Array",
        lambda: test_array(conn),
        unsupported=True,
    )

    test(
        "Array parameter",
        lambda: test_array_parameter(conn),
        unsupported=True,
    )


    section("EXPLAIN")

    test(
        "EXPLAIN",
        lambda: test_explain(conn),
        unsupported=True,
    )


    section("POSTGRES CATALOG")

    test(
        "pg_type",
        lambda: test_pg_type(conn),
        unsupported=True,
    )

    test(
        "pg_class",
        lambda: test_pg_class(conn),
        unsupported=True,
    )

    test(
        "pg_namespace",
        lambda: test_pg_namespace(conn),
        unsupported=True,
    )

    test(
        "information_schema",
        lambda: test_information_schema(conn),
        unsupported=True,
    )

    test(
        "current_database()",
        lambda: test_current_database(conn)
    )

    test(
        "current_schema()",
        lambda: test_current_schema(conn)
    )

    test(
        "version()",
        lambda: test_version(conn)
    )


    section("ERROR HANDLING")

    test(
        "Duplicate primary key",
        lambda: test_duplicate_primary_key(conn)
    )

    test(
        "NOT NULL violation",
        lambda: test_not_null(conn)
    )

    test(
        "Syntax error",
        lambda: test_invalid_sql(conn)
    )

    test(
        "Undefined table",
        lambda: test_undefined_table(conn)
    )

    test(
        "Undefined column",
        lambda: test_undefined_column(conn)
    )


    section("CURSOR / RESULT HANDLING")

    test(
        "fetchone()",
        lambda: test_fetchone(conn)
    )

    test(
        "fetchmany()",
        lambda: test_fetchmany(conn)
    )

    test(
        "cursor.description",
        lambda: test_description(conn)
    )


    section("SESSION PARAMETERS")

    test(
        "SHOW server_version",
        lambda: test_show_server_version(conn),
        unsupported=True,
    )

    test(
        "SHOW client_encoding",
        lambda: test_show_client_encoding(conn),
        unsupported=True,
    )

    test(
        "SHOW TimeZone",
        lambda: test_show_timezone(conn),
        unsupported=True,
    )


    section("TRANSACTION ERROR RECOVERY")

    test(
        "Failed transaction recovery",
        lambda: test_failed_transaction_recovery(conn)
    )


    section("OPTIONAL POSTGRES FEATURES")

    test(
        "LISTEN / NOTIFY",
        lambda: test_listen_notify(conn),
        unsupported=True,
    )

    test(
        "COPY FROM STDIN",
        lambda: test_copy_feature(conn),
        unsupported=True,
    )


# ============================================================
# MAIN
# ============================================================

def main():

    title("PLOMID POSTGRESQL COMPATIBILITY TEST")

    print("\nDSN:")
    print(CONNECTION_STRING)

    print("\nSeparate credentials:")
    print(f"  Host:     {DB_HOST}")
    print(f"  Port:     {DB_PORT}")
    print(f"  Database: {DB_NAME}")
    print(f"  User:     {DB_USER}")
    print("  Password: ********")


    # --------------------------------------------------------
    # Test connection string
    # --------------------------------------------------------

    test(
        "Connection using PostgreSQL connection string",
        test_dsn_connection,
    )


    # --------------------------------------------------------
    # Test separate credentials
    # --------------------------------------------------------

    test(
        "Connection using separate credentials",
        test_separate_credentials_connection,
    )


    # --------------------------------------------------------
    # Main connection
    # --------------------------------------------------------

    print("\nOpening main test connection...")

    global conn
    try:
        conn = connect_using_dsn()

    except Exception as exc:
        print("\n✗ Could not connect to Plomid")
        print(f"{type(exc).__name__}: {exc}")
        sys.exit(1)


    print("✓ Main connection established")


    try:
        run_suite(conn)

    except KeyboardInterrupt:
        print("\n\nInterrupted.")

    except Exception as exc:
        print("\nUnexpected test-suite error:")
        print(f"{type(exc).__name__}: {exc}")

        traceback.print_exc()

    finally:
        cleanup(conn)
        conn.close()


    # --------------------------------------------------------
    # Summary
    # --------------------------------------------------------

    title("FINAL RESULT")

    print(f"PASSED:       {PASSED}")
    print(f"FAILED:       {FAILED}")
    print(f"UNSUPPORTED:  {UNSUPPORTED}")

    total = PASSED + FAILED + UNSUPPORTED

    print(f"TOTAL:        {total}")

    if FAILED == 0:
        print("\n✓ NO TEST FAILURES")
    else:
        print("\n✗ COMPATIBILITY FAILURES DETECTED")

    print("\n")


if __name__ == "__main__":
    main()
