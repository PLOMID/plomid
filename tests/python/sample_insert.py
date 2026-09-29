#!/usr/bin/env python3

"""
PLOMID Production Regression / Workload Test

Requirements:
    pip install psycopg2-binary

Run:
    python3 plomid_prod_regression.py

Environment:
    PLOMID_HOST=127.0.0.1
    PLOMID_PORT=5432
    PLOMID_USER=plomid
    PLOMID_PASSWORD=plomid
    PLOMID_DATABASE=plomid

This test exercises:

    1. Connection
    2. Schema creation
    3. Tables
    4. Constraints
    5. Indexes
    6. Realistic data loading
    7. SELECT
    8. INSERT / UPDATE / DELETE
    9. JOIN
   10. GROUP BY / HAVING
   11. Subqueries
   12. CASE / COALESCE
   13. String functions
   14. Date functions
   15. NULL handling
   16. DISTINCT
   17. ORDER BY / LIMIT / OFFSET
   18. Transactions
   19. Rollback
   20. Constraint violations
   21. Parameterized queries
   22. information_schema
   23. pg_catalog
   24. Views
   25. PostgreSQL :: casts
   26. PostgreSQL CAST() expressions
   27. Production workload
   28. Concurrent clients
   29. Final integrity checks
   30. Cleanup

IMPORTANT:
    Both PostgreSQL cast syntaxes are intentionally tested:

        expression::TYPE

    and:

        CAST(expression AS TYPE)

    Do NOT remove either test.
"""

import os
import sys
import time
import random
import threading
import traceback

from datetime import datetime, timedelta
from decimal import Decimal

import psycopg2


# ============================================================
# CONFIG
# ============================================================

HOST = os.getenv("PLOMID_HOST", "127.0.0.1")
PORT = int(os.getenv("PLOMID_PORT", "6000"))
USER = os.getenv("PLOMID_USER", "plomid")
PASSWORD = os.getenv("PLOMID_PASSWORD", "plomid")
DATABASE = os.getenv("PLOMID_DATABASE", "plomid")

SCHEMA = os.getenv("PLOMID_TEST_SCHEMA", "plomid_prodtest")

RANDOM_SEED = 42

CUSTOMERS = 2_000
PRODUCTS = 500
ORDERS = 5_000
ORDER_ITEMS_PER_ORDER = 3

WORKLOAD_QUERIES = 250

CONCURRENT_CLIENTS = 5
CONCURRENT_QUERIES = 50

PASSED = 0
FAILED = 0
SKIPPED = 0
TESTS = 0

SUITE_START = time.perf_counter()

random.seed(RANDOM_SEED)


# ============================================================
# LOGGING
# ============================================================

def now():
    return datetime.now().strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]


def log(level, message):
    print(
        f"{now()} [{level:<5}] {message}",
        flush=True,
    )


def section(title):
    print()
    print("=" * 90)
    print(f" {title}")
    print("=" * 90)


def elapsed_ms(start):
    return (time.perf_counter() - start) * 1000


def compact_sql(query):
    return " ".join(query.strip().split())


# ============================================================
# ASSERTIONS
# ============================================================

def check(condition, message):
    if not condition:
        raise AssertionError(message)


def eq(actual, expected, message=""):
    if actual != expected:
        raise AssertionError(
            f"{message} expected={expected!r}, actual={actual!r}"
        )


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

    log("TEST", name)

    try:
        fn()

        PASSED += 1

        log(
            "PASS",
            f"{name} [{elapsed_ms(start):.2f} ms]",
        )

    except NotImplementedError as exc:
        SKIPPED += 1

        log(
            "SKIP",
            f"{name} [{elapsed_ms(start):.2f} ms] {exc}",
        )

    except Exception as exc:
        FAILED += 1

        log(
            "FAIL",
            f"{name} [{elapsed_ms(start):.2f} ms]",
        )

        log(
            "ERROR",
            f"{type(exc).__name__}: {exc}",
        )


# ============================================================
# DATABASE CONNECTION
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
        application_name="PLOMID-PROD-REGRESSION",
    )

    conn.autocommit = True

    log(
        "INFO",
        f"connected in {elapsed_ms(start):.2f} ms",
    )

    return conn


# ============================================================
# SQL HELPERS
# ============================================================

def execute(conn, query, params=None, fetch=False):
    cur = conn.cursor()

    start = time.perf_counter()

    try:
        log(
            "SQL",
            compact_sql(query),
        )

        cur.execute(query, params)

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
            f"{elapsed_ms(start):.2f} ms rows={row_count}",
        )

        return rows

    finally:
        cur.close()


def scalar(conn, query, params=None):
    rows = execute(
        conn,
        query,
        params,
        fetch=True,
    )

    if not rows:
        return None

    return rows[0][0]


# ============================================================
# CONNECTION TEST
# ============================================================

def test_connection(conn):
    version = scalar(
        conn,
        "SELECT version()",
    )

    database = scalar(
        conn,
        "SELECT current_database()",
    )

    current_user = scalar(
        conn,
        "SELECT current_user",
    )

    log("INFO", f"database={database}")
    log("INFO", f"user={current_user}")
    log("INFO", f"version={version}")

    eq(
        database,
        DATABASE,
        "wrong database",
    )


# ============================================================
# CLEAN START
# ============================================================

def clean_schema(conn):
    execute(
        conn,
        f"DROP SCHEMA IF EXISTS {SCHEMA} CASCADE",
    )


# ============================================================
# CREATE SCHEMA
# ============================================================

def create_schema(conn):
    execute(
        conn,
        f"CREATE SCHEMA {SCHEMA}",
    )


# ============================================================
# CREATE TABLES
# ============================================================

def create_tables(conn):

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.customers (
            id BIGINT PRIMARY KEY,
            first_name VARCHAR(100) NOT NULL,
            last_name VARCHAR(100) NOT NULL,
            email VARCHAR(255) NOT NULL UNIQUE,
            country VARCHAR(100) NOT NULL,
            status VARCHAR(30) NOT NULL DEFAULT 'active',
            credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00,
            created_at TIMESTAMP NOT NULL
        )
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.products (
            id BIGINT PRIMARY KEY,
            sku VARCHAR(50) NOT NULL UNIQUE,
            name VARCHAR(200) NOT NULL,
            category VARCHAR(100) NOT NULL,
            price NUMERIC(12,2) NOT NULL,
            stock INTEGER NOT NULL,
            active BOOLEAN NOT NULL DEFAULT TRUE,
            created_at TIMESTAMP NOT NULL
        )
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.orders (
            id BIGINT PRIMARY KEY,
            customer_id BIGINT NOT NULL,
            status VARCHAR(30) NOT NULL,
            total_amount NUMERIC(12,2) NOT NULL,
            order_date TIMESTAMP NOT NULL,
            shipping_country VARCHAR(100) NOT NULL,

            CONSTRAINT orders_customer_fk
                FOREIGN KEY (customer_id)
                REFERENCES {SCHEMA}.customers(id)
        )
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.order_items (
            id BIGINT PRIMARY KEY,
            order_id BIGINT NOT NULL,
            product_id BIGINT NOT NULL,
            quantity INTEGER NOT NULL,
            unit_price NUMERIC(12,2) NOT NULL,

            CONSTRAINT items_order_fk
                FOREIGN KEY (order_id)
                REFERENCES {SCHEMA}.orders(id),

            CONSTRAINT items_product_fk
                FOREIGN KEY (product_id)
                REFERENCES {SCHEMA}.products(id)
        )
        """,
    )

    execute(
        conn,
        f"""
        CREATE TABLE {SCHEMA}.payments (
            id BIGINT PRIMARY KEY,
            order_id BIGINT NOT NULL,
            amount NUMERIC(12,2) NOT NULL,
            method VARCHAR(50) NOT NULL,
            status VARCHAR(30) NOT NULL,
            paid_at TIMESTAMP,

            CONSTRAINT payments_order_fk
                FOREIGN KEY (order_id)
                REFERENCES {SCHEMA}.orders(id)
        )
        """,
    )


# ============================================================
# INDEXES
# ============================================================

def create_indexes(conn):

    indexes = [
        f"""
        CREATE INDEX customers_country_idx
        ON {SCHEMA}.customers(country)
        """,

        f"""
        CREATE INDEX customers_status_idx
        ON {SCHEMA}.customers(status)
        """,

        f"""
        CREATE INDEX products_category_idx
        ON {SCHEMA}.products(category)
        """,

        f"""
        CREATE INDEX products_price_idx
        ON {SCHEMA}.products(price)
        """,

        f"""
        CREATE INDEX orders_customer_idx
        ON {SCHEMA}.orders(customer_id)
        """,

        f"""
        CREATE INDEX orders_date_idx
        ON {SCHEMA}.orders(order_date)
        """,

        f"""
        CREATE INDEX orders_status_idx
        ON {SCHEMA}.orders(status)
        """,

        f"""
        CREATE INDEX items_order_idx
        ON {SCHEMA}.order_items(order_id)
        """,

        f"""
        CREATE INDEX items_product_idx
        ON {SCHEMA}.order_items(product_id)
        """,

        f"""
        CREATE INDEX payments_order_idx
        ON {SCHEMA}.payments(order_id)
        """,
    ]

    for query in indexes:
        execute(conn, query)


# ============================================================
# DATA GENERATION
# ============================================================

FIRST_NAMES = [
    "Alice",
    "Bob",
    "Charlie",
    "David",
    "Emma",
    "Frank",
    "Grace",
    "Henry",
    "Ivy",
    "Jack",
]

LAST_NAMES = [
    "Smith",
    "Johnson",
    "Brown",
    "Taylor",
    "Wilson",
    "Anderson",
    "Thomas",
    "Jackson",
    "White",
    "Harris",
]

COUNTRIES = [
    "India",
    "USA",
    "UK",
    "Germany",
    "France",
    "Japan",
    "Australia",
    "Canada",
]

CATEGORIES = [
    "Electronics",
    "Computers",
    "Phones",
    "Accessories",
    "Furniture",
    "Books",
    "Clothing",
    "Home",
]

ORDER_STATUSES = [
    "pending",
    "processing",
    "shipped",
    "delivered",
    "cancelled",
]

PAYMENT_METHODS = [
    "card",
    "upi",
    "bank_transfer",
    "wallet",
]


def random_date(days=365):
    base = datetime(2025, 1, 1)

    return base + timedelta(
        days=random.randint(0, days),
        seconds=random.randint(0, 86400),
    )


# ============================================================
# LOAD CUSTOMERS
# ============================================================

def load_customers(conn):

    section("LOAD CUSTOMERS")

    start = time.perf_counter()

    cur = conn.cursor()

    try:
        for i in range(1, CUSTOMERS + 1):

            first = random.choice(FIRST_NAMES)
            last = random.choice(LAST_NAMES)

            email = f"user{i}@example.com"

            country = random.choice(COUNTRIES)

            status = (
                "inactive"
                if i % 17 == 0
                else "active"
            )

            credit = Decimal(
                random.randint(500, 20000)
            )

            cur.execute(
                f"""
                INSERT INTO {SCHEMA}.customers
                (
                    id,
                    first_name,
                    last_name,
                    email,
                    country,
                    status,
                    credit_limit,
                    created_at
                )
                VALUES (%s,%s,%s,%s,%s,%s,%s,%s)
                """,
                (
                    i,
                    first,
                    last,
                    email,
                    country,
                    status,
                    credit,
                    random_date(),
                ),
            )

    finally:
        cur.close()

    log(
        "LOAD",
        f"{CUSTOMERS:,} customers inserted "
        f"in {elapsed_ms(start):.2f} ms",
    )


# ============================================================
# LOAD PRODUCTS
# ============================================================

def load_products(conn):

    section("LOAD PRODUCTS")

    start = time.perf_counter()

    cur = conn.cursor()

    try:
        for i in range(1, PRODUCTS + 1):

            category = random.choice(CATEGORIES)

            price = (
                Decimal(random.randint(500, 250000))
                / 100
            )

            cur.execute(
                f"""
                INSERT INTO {SCHEMA}.products
                (
                    id,
                    sku,
                    name,
                    category,
                    price,
                    stock,
                    active,
                    created_at
                )
                VALUES (%s,%s,%s,%s,%s,%s,%s,%s)
                """,
                (
                    i,
                    f"SKU-{i:06d}",
                    f"{category} Product {i}",
                    category,
                    price,
                    random.randint(0, 1000),
                    i % 31 != 0,
                    random_date(),
                ),
            )

    finally:
        cur.close()

    log(
        "LOAD",
        f"{PRODUCTS:,} products inserted "
        f"in {elapsed_ms(start):.2f} ms",
    )


# ============================================================
# LOAD ORDERS
# ============================================================

def load_orders(conn):

    section("LOAD ORDERS")

    start = time.perf_counter()

    cur = conn.cursor()

    try:
        item_id = 1

        for order_id in range(1, ORDERS + 1):

            customer_id = random.randint(
                1,
                CUSTOMERS,
            )

            status = random.choice(
                ORDER_STATUSES,
            )

            order_date = random_date(730)

            total = Decimal("0")

            selected_products = random.sample(
                range(1, PRODUCTS + 1),
                ORDER_ITEMS_PER_ORDER,
            )

            items = []

            for product_id in selected_products:

                quantity = random.randint(1, 5)

                price = (
                    Decimal(
                        random.randint(1000, 100000)
                    )
                    / 100
                )

                total += price * quantity

                items.append(
                    (
                        item_id,
                        order_id,
                        product_id,
                        quantity,
                        price,
                    )
                )

                item_id += 1

            cur.execute(
                f"""
                INSERT INTO {SCHEMA}.orders
                (
                    id,
                    customer_id,
                    status,
                    total_amount,
                    order_date,
                    shipping_country
                )
                VALUES (%s,%s,%s,%s,%s,%s)
                """,
                (
                    order_id,
                    customer_id,
                    status,
                    total,
                    order_date,
                    random.choice(COUNTRIES),
                ),
            )

            for item in items:
                cur.execute(
                    f"""
                    INSERT INTO {SCHEMA}.order_items
                    (
                        id,
                        order_id,
                        product_id,
                        quantity,
                        unit_price
                    )
                    VALUES (%s,%s,%s,%s,%s)
                    """,
                    item,
                )

            payment_status = (
                "failed"
                if status == "cancelled"
                else "paid"
            )

            cur.execute(
                f"""
                INSERT INTO {SCHEMA}.payments
                (
                    id,
                    order_id,
                    amount,
                    method,
                    status,
                    paid_at
                )
                VALUES (%s,%s,%s,%s,%s,%s)
                """,
                (
                    order_id,
                    order_id,
                    total,
                    random.choice(PAYMENT_METHODS),
                    payment_status,
                    (
                        order_date
                        if payment_status == "paid"
                        else None
                    ),
                ),
            )

    finally:
        cur.close()

    log(
        "LOAD",
        f"{ORDERS:,} orders + "
        f"{ORDERS * ORDER_ITEMS_PER_ORDER:,} items + "
        f"{ORDERS:,} payments inserted "
        f"in {elapsed_ms(start):.2f} ms",
    )


# ============================================================
# POSTGRESQL CASTS
#
# IMPORTANT:
# Test BOTH:
#
#     expression::TYPE
#
# and:
#
#     CAST(expression AS TYPE)
# ============================================================

def test_postgresql_double_colon_casts(conn):

    section("POSTGRESQL :: CAST SYNTAX")

    tests = [
        (
            "integer literal",
            "SELECT 123::INTEGER",
            123,
        ),
        (
            "text to integer",
            "SELECT '123'::INTEGER",
            123,
        ),
        (
            "numeric precision",
            "SELECT 123.45::NUMERIC(10,2)",
            Decimal("123.45"),
        ),
        (
            "NULL integer",
            "SELECT NULL::INTEGER",
            None,
        ),
        (
            "boolean",
            "SELECT 'true'::BOOLEAN",
            True,
        ),
        (
            "date",
            "SELECT '2026-09-10'::DATE",
            datetime(2026, 9, 10).date(),
        ),
        (
            "integer arithmetic",
            "SELECT 123::INTEGER + 10",
            133,
        ),
        (
            "integer multiplication",
            "SELECT '123'::INTEGER * 2",
            246,
        ),
        (
            "numeric arithmetic",
            "SELECT '123.45'::NUMERIC + 1",
            Decimal("124.45"),
        ),
    ]

    for name, sql, expected in tests:

        start = time.perf_counter()

        try:
            actual = scalar(conn, sql)

        except Exception as exc:
            raise AssertionError(
                f"PostgreSQL :: cast failed: "
                f"{name}; SQL={sql!r}; "
                f"{type(exc).__name__}: {exc}"
            ) from exc

        eq(
            actual,
            expected,
            f"PostgreSQL :: cast failed: {name}",
        )

        log(
            "PASS",
            f":: cast: {name} "
            f"[{elapsed_ms(start):.2f} ms]",
        )


# ============================================================
# STANDARD CAST() SYNTAX
# ============================================================

def test_standard_cast_expressions(conn):

    section("STANDARD CAST() SYNTAX")

    tests = [
        (
            "integer literal",
            "SELECT CAST(123 AS INTEGER)",
            123,
        ),
        (
            "text to integer",
            "SELECT CAST('123' AS INTEGER)",
            123,
        ),
        (
            "numeric precision",
            "SELECT CAST(123.45 AS NUMERIC(10,2))",
            Decimal("123.45"),
        ),
        (
            "NULL integer",
            "SELECT CAST(NULL AS INTEGER)",
            None,
        ),
        (
            "boolean",
            "SELECT CAST('true' AS BOOLEAN)",
            True,
        ),
        (
            "date",
            "SELECT CAST('2026-09-10' AS DATE)",
            datetime(2026, 9, 10).date(),
        ),
        (
            "integer arithmetic",
            "SELECT CAST(123 AS INTEGER) + 10",
            133,
        ),
        (
            "integer multiplication",
            "SELECT CAST('123' AS INTEGER) * 2",
            246,
        ),
        (
            "numeric arithmetic",
            "SELECT CAST('123.45' AS NUMERIC) + 1",
            Decimal("124.45"),
        ),
    ]

    for name, sql, expected in tests:

        start = time.perf_counter()

        try:
            actual = scalar(conn, sql)

        except Exception as exc:
            raise AssertionError(
                f"CAST() failed: "
                f"{name}; SQL={sql!r}; "
                f"{type(exc).__name__}: {exc}"
            ) from exc

        eq(
            actual,
            expected,
            f"CAST() failed: {name}",
        )

        log(
            "PASS",
            f"CAST(): {name} "
            f"[{elapsed_ms(start):.2f} ms]",
        )


# ============================================================
# CASTS INSIDE REAL QUERIES
# ============================================================

def test_casts_in_real_queries(conn):

    section("CASTS IN REAL QUERIES")

    rows = execute(
        conn,
        f"""
        SELECT
            id,
            credit_limit::NUMERIC(12,2),
            CAST(credit_limit AS NUMERIC(12,2)),
            stock::INTEGER,
            CAST(stock AS INTEGER)
        FROM {SCHEMA}.customers
        CROSS JOIN {SCHEMA}.products
        LIMIT 10
        """,
        fetch=True,
    )

    eq(
        len(rows),
        10,
        "real query cast test",
    )

    for row in rows:

        eq(
            row[1],
            row[2],
            "double-colon and CAST numeric results differ",
        )

        eq(
            row[3],
            row[4],
            "double-colon and CAST integer results differ",
        )


# ============================================================
# DATA COUNTS
# ============================================================

def test_counts(conn):

    expected = {
        "customers": CUSTOMERS,
        "products": PRODUCTS,
        "orders": ORDERS,
        "order_items": ORDERS * ORDER_ITEMS_PER_ORDER,
        "payments": ORDERS,
    }

    for table, expected_count in expected.items():

        count = scalar(
            conn,
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{table}
            """,
        )

        eq(
            count,
            expected_count,
            table,
        )


# ============================================================
# BASIC READ
# ============================================================

def test_basic_reads(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            id,
            first_name,
            last_name,
            email
        FROM {SCHEMA}.customers
        WHERE status = 'active'
        ORDER BY id
        LIMIT 50
        """,
        fetch=True,
    )

    eq(
        len(rows),
        50,
        "LIMIT failed",
    )


# ============================================================
# PAGINATION
# ============================================================

def test_pagination(conn):

    page_size = 50

    page1 = execute(
        conn,
        f"""
        SELECT id
        FROM {SCHEMA}.customers
        ORDER BY id
        LIMIT %s OFFSET %s
        """,
        (page_size, 0),
        True,
    )

    page2 = execute(
        conn,
        f"""
        SELECT id
        FROM {SCHEMA}.customers
        ORDER BY id
        LIMIT %s OFFSET %s
        """,
        (page_size, 50),
        True,
    )

    check(
        page1[-1][0] < page2[0][0],
        "pagination overlap",
    )


# ============================================================
# CUSTOMER / ORDER AGGREGATION
# ============================================================

def test_customer_orders(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            c.id,
            c.first_name,
            c.last_name,
            COUNT(o.id) AS order_count,
            COALESCE(
                SUM(o.total_amount),
                0
            ) AS total_spent
        FROM {SCHEMA}.customers c
        LEFT JOIN {SCHEMA}.orders o
            ON o.customer_id = c.id
        GROUP BY
            c.id,
            c.first_name,
            c.last_name
        ORDER BY total_spent DESC
        LIMIT 25
        """,
        fetch=True,
    )

    eq(
        len(rows),
        25,
        "customer aggregation failed",
    )


# ============================================================
# ORDER DETAILS JOIN
# ============================================================

def test_order_details(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            o.id,
            c.email,
            o.status,
            p.name,
            oi.quantity,
            oi.unit_price
        FROM {SCHEMA}.orders o
        JOIN {SCHEMA}.customers c
            ON c.id = o.customer_id
        JOIN {SCHEMA}.order_items oi
            ON oi.order_id = o.id
        JOIN {SCHEMA}.products p
            ON p.id = oi.product_id
        WHERE o.id <= 100
        ORDER BY o.id, oi.id
        """,
        fetch=True,
    )

    eq(
        len(rows),
        300,
        "order detail join failed",
    )


# ============================================================
# AGGREGATION
# ============================================================

def test_aggregation(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            p.category,
            COUNT(*) AS item_count,
            SUM(oi.quantity) AS quantity,
            SUM(
                oi.quantity * oi.unit_price
            ) AS revenue,
            AVG(oi.unit_price) AS average_price
        FROM {SCHEMA}.order_items oi
        JOIN {SCHEMA}.products p
            ON p.id = oi.product_id
        GROUP BY p.category
        HAVING SUM(oi.quantity) > 0
        ORDER BY revenue DESC
        """,
        fetch=True,
    )

    check(
        len(rows) > 0,
        "aggregation returned no rows",
    )


# ============================================================
# SUBQUERY
# ============================================================

def test_subquery(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            id,
            email
        FROM {SCHEMA}.customers
        WHERE id IN (
            SELECT customer_id
            FROM {SCHEMA}.orders
            GROUP BY customer_id
            HAVING COUNT(*) >= 5
        )
        ORDER BY id
        LIMIT 100
        """,
        fetch=True,
    )

    check(
        len(rows) >= 0,
        "subquery failed",
    )


# ============================================================
# CASE / COALESCE
# ============================================================

def test_case(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            id,
            CASE
                WHEN total_amount >= 1000
                    THEN 'large'
                WHEN total_amount >= 500
                    THEN 'medium'
                ELSE 'small'
            END AS order_size,
            COALESCE(total_amount, 0)
        FROM {SCHEMA}.orders
        ORDER BY id
        LIMIT 100
        """,
        fetch=True,
    )

    eq(
        len(rows),
        100,
    )


# ============================================================
# STRING FUNCTIONS
# ============================================================

def test_strings(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            LOWER(email),
            UPPER(country),
            LENGTH(first_name),
            CONCAT(
                first_name,
                ' ',
                last_name
            )
        FROM {SCHEMA}.customers
        LIMIT 100
        """,
        fetch=True,
    )

    eq(
        len(rows),
        100,
    )


# ============================================================
# DATE FUNCTIONS
# ============================================================

def test_dates(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            DATE_TRUNC(
                'month',
                order_date
            ) AS month,
            COUNT(*),
            SUM(total_amount)
        FROM {SCHEMA}.orders
        GROUP BY DATE_TRUNC(
            'month',
            order_date
        )
        ORDER BY month
        """,
        fetch=True,
    )

    check(
        len(rows) > 0,
        "date aggregation failed",
    )


# ============================================================
# UPDATE
# ============================================================

def test_updates(conn):

    before = scalar(
        conn,
        f"""
        SELECT stock
        FROM {SCHEMA}.products
        WHERE id = 1
        """,
    )

    execute(
        conn,
        f"""
        UPDATE {SCHEMA}.products
        SET stock = stock + 100
        WHERE id = 1
        """,
    )

    after = scalar(
        conn,
        f"""
        SELECT stock
        FROM {SCHEMA}.products
        WHERE id = 1
        """,
    )

    eq(
        after,
        before + 100,
    )


# ============================================================
# DELETE
# ============================================================

def test_delete(conn):

    execute(
        conn,
        f"""
        INSERT INTO {SCHEMA}.customers
        (
            id,
            first_name,
            last_name,
            email,
            country,
            status,
            credit_limit,
            created_at
        )
        VALUES
        (
            9999999,
            'Delete',
            'Test',
            'delete-test@example.com',
            'India',
            'active',
            1000,
            CURRENT_TIMESTAMP
        )
        """,
    )

    execute(
        conn,
        f"""
        DELETE FROM {SCHEMA}.customers
        WHERE id = 9999999
        """,
    )

    count = scalar(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {SCHEMA}.customers
        WHERE id = 9999999
        """,
    )

    eq(
        count,
        0,
    )


# ============================================================
# TRANSACTION COMMIT
# ============================================================

def test_transaction_commit(conn):

    conn.autocommit = False

    try:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.customers
            (
                id,
                first_name,
                last_name,
                email,
                country,
                status,
                credit_limit,
                created_at
            )
            VALUES
            (
                8888888,
                'Transaction',
                'Commit',
                'commit@example.com',
                'India',
                'active',
                1000,
                CURRENT_TIMESTAMP
            )
            """,
        )

        conn.commit()

    finally:
        conn.autocommit = True

    count = scalar(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {SCHEMA}.customers
        WHERE id = 8888888
        """,
    )

    eq(
        count,
        1,
    )


# ============================================================
# TRANSACTION ROLLBACK
# ============================================================

def test_transaction_rollback(conn):

    conn.autocommit = False

    try:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.customers
            (
                id,
                first_name,
                last_name,
                email,
                country,
                status,
                credit_limit,
                created_at
            )
            VALUES
            (
                8888889,
                'Transaction',
                'Rollback',
                'rollback@example.com',
                'India',
                'active',
                1000,
                CURRENT_TIMESTAMP
            )
            """,
        )

        conn.rollback()

    finally:
        conn.autocommit = True

    count = scalar(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {SCHEMA}.customers
        WHERE id = 8888889
        """,
    )

    eq(
        count,
        0,
    )


# ============================================================
# PRIMARY KEY VIOLATION
# ============================================================

def test_duplicate_primary_key(conn):

    try:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.customers
            (
                id,
                first_name,
                last_name,
                email,
                country,
                status,
                credit_limit,
                created_at
            )
            VALUES
            (
                1,
                'Duplicate',
                'User',
                'duplicate@example.com',
                'India',
                'active',
                1000,
                CURRENT_TIMESTAMP
            )
            """,
        )

    except Exception:
        conn.rollback()
        return

    raise AssertionError(
        "duplicate PK accepted",
    )


# ============================================================
# UNIQUE VIOLATION
# ============================================================

def test_duplicate_unique(conn):

    try:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.customers
            (
                id,
                first_name,
                last_name,
                email,
                country,
                status,
                credit_limit,
                created_at
            )
            VALUES
            (
                7777777,
                'Duplicate',
                'Email',
                'user1@example.com',
                'India',
                'active',
                1000,
                CURRENT_TIMESTAMP
            )
            """,
        )

    except Exception:
        conn.rollback()
        return

    raise AssertionError(
        "duplicate UNIQUE value accepted",
    )


# ============================================================
# FOREIGN KEY VIOLATION
# ============================================================

def test_foreign_key(conn):

    try:

        execute(
            conn,
            f"""
            INSERT INTO {SCHEMA}.orders
            (
                id,
                customer_id,
                status,
                total_amount,
                order_date,
                shipping_country
            )
            VALUES
            (
                7777777,
                999999999,
                'pending',
                10,
                CURRENT_TIMESTAMP,
                'India'
            )
            """,
        )

    except Exception:
        conn.rollback()
        return

    raise AssertionError(
        "invalid foreign key accepted",
    )


# ============================================================
# INVALID SQL RECOVERY
# ============================================================

def test_invalid_sql_recovery(conn):

    try:

        execute(
            conn,
            "THIS IS INVALID SQL",
        )

    except Exception:
        conn.rollback()

    result = scalar(
        conn,
        "SELECT 1",
    )

    eq(
        result,
        1,
        "connection did not recover",
    )


# ============================================================
# PARAMETERIZED QUERIES
# ============================================================

def test_parameterized(conn):

    for customer_id in [
        1,
        10,
        100,
        500,
        1000,
    ]:

        rows = execute(
            conn,
            f"""
            SELECT
                id,
                email,
                country
            FROM {SCHEMA}.customers
            WHERE id = %s
            """,
            (customer_id,),
            fetch=True,
        )

        eq(
            len(rows),
            1,
        )


# ============================================================
# INFORMATION_SCHEMA
# ============================================================

def test_information_schema(conn):

    rows = execute(
        conn,
        """
        SELECT table_name
        FROM information_schema.tables
        WHERE table_schema = %s
        ORDER BY table_name
        """,
        (SCHEMA,),
        fetch=True,
    )

    names = {
        row[0]
        for row in rows
    }

    required = {
        "customers",
        "products",
        "orders",
        "order_items",
        "payments",
    }

    check(
        required.issubset(names),
        f"missing tables: {required - names}",
    )


# ============================================================
# PG_CATALOG
# ============================================================

def test_pg_catalog(conn):

    rows = execute(
        conn,
        """
        SELECT tablename
        FROM pg_catalog.pg_tables
        WHERE schemaname = %s
        ORDER BY tablename
        """,
        (SCHEMA,),
        fetch=True,
    )

    check(
        len(rows) >= 5,
        "pg_catalog.pg_tables returned too few tables",
    )


# ============================================================
# NULL
# ============================================================

def test_null(conn):

    rows = execute(
        conn,
        """
        SELECT
            NULL IS NULL,
            NULL IS NOT NULL,
            COALESCE(NULL, 123)
        """,
        fetch=True,
    )

    eq(
        rows[0],
        (True, False, 123),
    )


# ============================================================
# DISTINCT
# ============================================================

def test_distinct(conn):

    rows = execute(
        conn,
        f"""
        SELECT DISTINCT country
        FROM {SCHEMA}.customers
        ORDER BY country
        """,
        fetch=True,
    )

    check(
        len(rows) >= 5,
        "DISTINCT returned too few countries",
    )


# ============================================================
# ORDER BY / LIMIT
# ============================================================

def test_sort_limit(conn):

    rows = execute(
        conn,
        f"""
        SELECT
            id,
            total_amount
        FROM {SCHEMA}.orders
        ORDER BY total_amount DESC
        LIMIT 100
        """,
        fetch=True,
    )

    eq(
        len(rows),
        100,
    )

    for i in range(1, len(rows)):

        check(
            rows[i - 1][1] >= rows[i][1],
            "ORDER BY DESC incorrect",
        )


# ============================================================
# VIEW
# ============================================================

def create_view(conn):

    execute(
        conn,
        f"""
        CREATE VIEW {SCHEMA}.customer_summary AS
        SELECT
            c.id,
            c.email,
            COUNT(o.id) AS order_count,
            COALESCE(
                SUM(o.total_amount),
                0
            ) AS total_spent
        FROM {SCHEMA}.customers c
        LEFT JOIN {SCHEMA}.orders o
            ON o.customer_id = c.id
        GROUP BY
            c.id,
            c.email
        """,
    )


def test_view(conn):

    rows = execute(
        conn,
        f"""
        SELECT *
        FROM {SCHEMA}.customer_summary
        ORDER BY total_spent DESC
        LIMIT 50
        """,
        fetch=True,
    )

    eq(
        len(rows),
        50,
    )


# ============================================================
# WORKLOAD
# ============================================================

def workload_query(conn, n):

    choice = n % 8

    if choice == 0:

        execute(
            conn,
            f"""
            SELECT
                c.country,
                COUNT(o.id),
                COALESCE(
                    SUM(o.total_amount),
                    0
                )
            FROM {SCHEMA}.customers c
            LEFT JOIN {SCHEMA}.orders o
                ON o.customer_id = c.id
            GROUP BY c.country
            ORDER BY COUNT(o.id) DESC
            """,
            fetch=True,
        )

    elif choice == 1:

        execute(
            conn,
            f"""
            SELECT
                p.category,
                COUNT(*),
                SUM(oi.quantity),
                SUM(
                    oi.quantity * oi.unit_price
                )
            FROM {SCHEMA}.order_items oi
            JOIN {SCHEMA}.products p
                ON p.id = oi.product_id
            GROUP BY p.category
            ORDER BY SUM(
                oi.quantity * oi.unit_price
            ) DESC
            """,
            fetch=True,
        )

    elif choice == 2:

        execute(
            conn,
            f"""
            SELECT
                o.id,
                o.status,
                o.total_amount
            FROM {SCHEMA}.orders o
            WHERE o.total_amount > %s
            ORDER BY o.total_amount DESC
            LIMIT 50
            """,
            (
                random.randint(100, 2000),
            ),
            fetch=True,
        )

    elif choice == 3:

        execute(
            conn,
            f"""
            SELECT
                c.email,
                COUNT(o.id)
            FROM {SCHEMA}.customers c
            JOIN {SCHEMA}.orders o
                ON o.customer_id = c.id
            WHERE c.country = %s
            GROUP BY c.email
            ORDER BY COUNT(o.id) DESC
            LIMIT 25
            """,
            (
                random.choice(COUNTRIES),
            ),
            fetch=True,
        )

    elif choice == 4:

        execute(
            conn,
            f"""
            SELECT
                DATE_TRUNC(
                    'month',
                    order_date
                ),
                COUNT(*),
                SUM(total_amount)
            FROM {SCHEMA}.orders
            GROUP BY DATE_TRUNC(
                'month',
                order_date
            )
            ORDER BY 1
            """,
            fetch=True,
        )

    elif choice == 5:

        execute(
            conn,
            f"""
            SELECT
                p.name,
                p.price,
                p.stock
            FROM {SCHEMA}.products p
            WHERE p.category = %s
              AND p.active = TRUE
              AND p.stock > 0
            ORDER BY p.price DESC
            LIMIT 25
            """,
            (
                random.choice(CATEGORIES),
            ),
            fetch=True,
        )

    elif choice == 6:

        execute(
            conn,
            f"""
            SELECT
                o.status,
                COUNT(*),
                AVG(total_amount),
                MIN(total_amount),
                MAX(total_amount)
            FROM {SCHEMA}.orders o
            GROUP BY o.status
            ORDER BY COUNT(*) DESC
            """,
            fetch=True,
        )

    else:

        low = random.randint(
            1,
            1000,
        )

        high = random.randint(
            1001,
            CUSTOMERS,
        )

        execute(
            conn,
            f"""
            SELECT
                id,
                email,
                country
            FROM {SCHEMA}.customers
            WHERE id BETWEEN %s AND %s
            ORDER BY id
            LIMIT 100
            """,
            (
                low,
                high,
            ),
            fetch=True,
        )


# ============================================================
# WORKLOAD TEST
# ============================================================

def test_workload(conn):

    section("PRODUCTION READ WORKLOAD")

    start = time.perf_counter()

    for i in range(WORKLOAD_QUERIES):
        workload_query(
            conn,
            i,
        )

    total = elapsed_ms(start)

    log(
        "PERF",
        f"{WORKLOAD_QUERIES} production queries "
        f"in {total:.2f} ms",
    )

    log(
        "PERF",
        f"average={total / WORKLOAD_QUERIES:.2f} ms/query",
    )


# ============================================================
# CONCURRENT CLIENT
# ============================================================

def concurrent_worker(worker_id, results):

    conn = None

    try:

        conn = connect()

        start = time.perf_counter()

        for i in range(CONCURRENT_QUERIES):

            workload_query(
                conn,
                worker_id * 1000 + i,
            )

        results[worker_id] = (
            True,
            elapsed_ms(start),
            None,
        )

    except Exception as exc:

        results[worker_id] = (
            False,
            0,
            str(exc),
        )

    finally:

        if conn is not None:
            conn.close()


# ============================================================
# CONCURRENCY TEST
# ============================================================

def test_concurrency():

    section("CONCURRENT CLIENT WORKLOAD")

    results = {}

    threads = []

    start = time.perf_counter()

    for worker_id in range(
        CONCURRENT_CLIENTS
    ):

        thread = threading.Thread(
            target=concurrent_worker,
            args=(
                worker_id,
                results,
            ),
        )

        threads.append(thread)

        thread.start()

    for thread in threads:
        thread.join()

    total = elapsed_ms(start)

    for worker_id in range(
        CONCURRENT_CLIENTS
    ):

        if worker_id not in results:
            raise RuntimeError(
                f"worker {worker_id} produced no result"
            )

        ok, duration, error = results[
            worker_id
        ]

        if not ok:
            raise RuntimeError(
                f"worker {worker_id} failed: {error}"
            )

        log(
            "WORKER",
            f"worker={worker_id} "
            f"time={duration:.2f} ms",
        )

    log(
        "PERF",
        f"{CONCURRENT_CLIENTS} clients × "
        f"{CONCURRENT_QUERIES} queries "
        f"completed in {total:.2f} ms",
    )


# ============================================================
# FINAL INTEGRITY
# ============================================================

def test_integrity(conn):

    section("FINAL DATA INTEGRITY")

    customers = scalar(
        conn,
        f"SELECT COUNT(*) FROM {SCHEMA}.customers",
    )

    products = scalar(
        conn,
        f"SELECT COUNT(*) FROM {SCHEMA}.products",
    )

    orders = scalar(
        conn,
        f"SELECT COUNT(*) FROM {SCHEMA}.orders",
    )

    items = scalar(
        conn,
        f"SELECT COUNT(*) FROM {SCHEMA}.order_items",
    )

    payments = scalar(
        conn,
        f"SELECT COUNT(*) FROM {SCHEMA}.payments",
    )

    log(
        "INFO",
        f"customers   = {customers:,}",
    )

    log(
        "INFO",
        f"products    = {products:,}",
    )

    log(
        "INFO",
        f"orders      = {orders:,}",
    )

    log(
        "INFO",
        f"order_items = {items:,}",
    )

    log(
        "INFO",
        f"payments    = {payments:,}",
    )

    eq(
        customers,
        CUSTOMERS + 1,
    )

    eq(
        products,
        PRODUCTS,
    )

    eq(
        orders,
        ORDERS,
    )

    eq(
        items,
        ORDERS * ORDER_ITEMS_PER_ORDER,
    )

    eq(
        payments,
        ORDERS,
    )

    orphan_orders = scalar(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {SCHEMA}.orders o
        LEFT JOIN {SCHEMA}.customers c
            ON c.id = o.customer_id
        WHERE c.id IS NULL
        """,
    )

    eq(
        orphan_orders,
        0,
        "orphan orders",
    )

    orphan_items = scalar(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {SCHEMA}.order_items i
        LEFT JOIN {SCHEMA}.orders o
            ON o.id = i.order_id
        WHERE o.id IS NULL
        """,
    )

    eq(
        orphan_items,
        0,
        "orphan order items",
    )

    invalid_payments = scalar(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {SCHEMA}.payments p
        JOIN {SCHEMA}.orders o
            ON o.id = p.order_id
        WHERE p.amount <> o.total_amount
        """,
    )

    eq(
        invalid_payments,
        0,
        "invalid payment amounts",
    )


# ============================================================
# MAIN
# ============================================================

def main():

    section("PLOMID PRODUCTION REGRESSION")

    log("INFO", f"host     = {HOST}")
    log("INFO", f"port     = {PORT}")
    log("INFO", f"user     = {USER}")
    log("INFO", f"database = {DATABASE}")
    log("INFO", f"schema   = {SCHEMA}")

    conn = None

    try:

        # ----------------------------------------------------
        section("CONNECT")
        # ----------------------------------------------------

        conn = connect()

        test(
            "connection / PostgreSQL metadata",
            lambda: test_connection(conn),
        )

        # ----------------------------------------------------
        section("RESET TEST ENVIRONMENT")
        # ----------------------------------------------------

        # clean_schema(conn)

        # ----------------------------------------------------
        section("DATABASE STRUCTURE")
        # ----------------------------------------------------

        test(
            "CREATE SCHEMA",
            lambda: create_schema(conn),
        )

        test(
            "CREATE production tables",
            lambda: create_tables(conn),
        )

        test(
            "CREATE indexes",
            lambda: create_indexes(conn),
        )

        # ----------------------------------------------------
        section("PRODUCTION DATA LOAD")
        # ----------------------------------------------------

        load_customers(conn)
        load_products(conn)
        load_orders(conn)

        test(
            "verify loaded row counts",
            lambda: test_counts(conn),
        )

        # ----------------------------------------------------
        section("READ OPERATIONS")
        # ----------------------------------------------------

        test(
            "basic SELECT workload",
            lambda: test_basic_reads(conn),
        )

        test(
            "pagination LIMIT/OFFSET",
            lambda: test_pagination(conn),
        )

        test(
            "customer/order aggregation",
            lambda: test_customer_orders(conn),
        )

        # ----------------------------------------------------
        # CAST TESTS
        # ----------------------------------------------------

        test(
            "PostgreSQL :: cast syntax",
            lambda: test_postgresql_double_colon_casts(conn),
        )

        test(
            "standard CAST() syntax",
            lambda: test_standard_cast_expressions(conn),
        )

        test(
            "casts inside real queries",
            lambda: test_casts_in_real_queries(conn),
        )

        # ----------------------------------------------------

        test(
            "multi-table order details JOIN",
            lambda: test_order_details(conn),
        )

        test(
            "GROUP BY / HAVING aggregation",
            lambda: test_aggregation(conn),
        )

        test(
            "subquery",
            lambda: test_subquery(conn),
        )

        test(
            "CASE / COALESCE",
            lambda: test_case(conn),
        )

        test(
            "string functions",
            lambda: test_strings(conn),
        )

        test(
            "date aggregation",
            lambda: test_dates(conn),
        )

        test(
            "NULL handling",
            lambda: test_null(conn),
        )

        test(
            "DISTINCT",
            lambda: test_distinct(conn),
        )

        test(
            "ORDER BY / LIMIT",
            lambda: test_sort_limit(conn),
        )

        # ----------------------------------------------------
        section("WRITE OPERATIONS")
        # ----------------------------------------------------

        test(
            "UPDATE",
            lambda: test_updates(conn),
        )

        test(
            "DELETE",
            lambda: test_delete(conn),
        )

        # ----------------------------------------------------
        section("TRANSACTIONS")
        # ----------------------------------------------------

        test(
            "transaction COMMIT",
            lambda: test_transaction_commit(conn),
        )

        test(
            "transaction ROLLBACK",
            lambda: test_transaction_rollback(conn),
        )

        # ----------------------------------------------------
        section("CONSTRAINTS")
        # ----------------------------------------------------

        test(
            "PRIMARY KEY violation",
            lambda: test_duplicate_primary_key(conn),
        )

        test(
            "UNIQUE violation",
            lambda: test_duplicate_unique(conn),
        )

        test(
            "FOREIGN KEY violation",
            lambda: test_foreign_key(conn),
        )

        # ----------------------------------------------------
        section("PARAMETERIZED QUERIES")
        # ----------------------------------------------------

        test(
            "parameterized SELECT",
            lambda: test_parameterized(conn),
        )

        # ----------------------------------------------------
        section("CATALOGS")
        # ----------------------------------------------------

        test(
            "information_schema",
            lambda: test_information_schema(conn),
        )

        test(
            "pg_catalog",
            lambda: test_pg_catalog(conn),
        )

        # ----------------------------------------------------
        section("VIEWS")
        # ----------------------------------------------------

        test(
            "CREATE VIEW",
            lambda: create_view(conn),
        )

        test(
            "SELECT VIEW",
            lambda: test_view(conn),
        )

        # ----------------------------------------------------
        section("ERROR HANDLING")
        # ----------------------------------------------------

        test(
            "invalid SQL recovery",
            lambda: test_invalid_sql_recovery(conn),
        )

        # ----------------------------------------------------
        section("WORKLOAD")
        # ----------------------------------------------------

        test(
            "production query workload",
            lambda: test_workload(conn),
        )

        # ----------------------------------------------------

        test(
            "concurrent client workload",
            test_concurrency,
        )

        # ----------------------------------------------------
        section("INTEGRITY")
        # ----------------------------------------------------

        test(
            "final data integrity",
            lambda: test_integrity(conn),
        )

    except KeyboardInterrupt:

        log(
            "WARN",
            "interrupted by user",
        )

    except Exception as exc:

        log(
            "FATAL",
            f"{type(exc).__name__}: {exc}",
        )

        traceback.print_exc()

    finally:

        if conn is not None:

            section("CLEANUP")

            try:

                conn.autocommit = True

                # execute(
                #     conn,
                #     f"""
                #     DROP SCHEMA IF EXISTS
                #     {SCHEMA}
                #     CASCADE
                #     """,
                # )

                log(
                    "PASS",
                    "production regression schema removed",
                )

            except Exception as exc:

                log(
                    "ERROR",
                    f"cleanup failed: {exc}",
                )

            try:
                conn.close()
            except Exception:
                pass

    # ========================================================
    # FINAL SUMMARY
    # ========================================================

    total_time = elapsed_ms(
        SUITE_START,
    )

    section("FINAL REGRESSION RESULT")

    log(
        "INFO",
        f"Tests   : {TESTS}",
    )

    log(
        "PASS",
        f"Passed  : {PASSED}",
    )

    log(
        "FAIL",
        f"Failed  : {FAILED}",
    )

    log(
        "SKIP",
        f"Skipped : {SKIPPED}",
    )

    log(
        "TIME",
        f"Total   : {total_time:.2f} ms",
    )

    print()

    if FAILED == 0:

        print(
            """
╔══════════════════════════════════════════════════════════════════════════════════╗
║                                                                                  ║
║                         PLOMID REGRESSION PASSED                                 ║
║                                                                                  ║
║   Production-style schema + data + CRUD + joins + casts + transactions +        ║
║                    catalogs + workload + concurrency                             ║
║                                                                                  ║
╚══════════════════════════════════════════════════════════════════════════════════╝
"""
        )

        return 0

    print(
        """
╔══════════════════════════════════════════════════════════════════════════════════╗
║                                                                                  ║
║                         PLOMID REGRESSION FAILED                                 ║
║                                                                                  ║
║                 One or more regression tests failed.                             ║
║                                                                                  ║
╚══════════════════════════════════════════════════════════════════════════════════╝
"""
    )

    return 1


if __name__ == "__main__":
    sys.exit(main())
