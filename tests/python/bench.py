#!/usr/bin/env python3

"""
===============================================================================
PLOMID vs PostgreSQL - 10M Scale Benchmark
===============================================================================

Databases
---------

PLOMID:
    postgresql://plomid:plomid@127.0.0.1:7000/plomid

PostgreSQL:
    postgresql://127.0.0.1:5432/postgres


IMPORTANT
---------

This benchmark does NOT assume PLOMID is faster.

It measures:

    - connection latency
    - schema creation
    - 10M-row insertion
    - primary-key lookup
    - unique-key lookup
    - indexed lookup
    - composite-index lookup
    - range scan
    - JSON extraction
    - JSON filtering
    - COUNT
    - aggregation
    - GROUP BY
    - DISTINCT
    - ORDER BY
    - JOIN
    - UPDATE
    - DELETE
    - transaction commit
    - transaction rollback
    - repeated-query latency
    - concurrent reads
    - concurrent writes
    - EXPLAIN
    - database size where supported

It records:

    - elapsed time
    - rows/sec
    - operations/sec
    - average latency
    - p50
    - p95
    - p99
    - min
    - max

Unsupported operations are recorded rather than silently treated as success.

At the end:

    - results are written to disk
    - Markdown report is generated
    - CSV comparison is generated
    - JSON results are generated
    - benchmark schema is dropped
    - cleanup is verified

Only the benchmark schema is modified.

===============================================================================
"""

import argparse
import csv
import json
import math
import os
import platform
import random
import statistics
import sys
import threading
import time
import traceback

from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timedelta, timezone

import psycopg2
from psycopg2.extras import execute_values


# =============================================================================
# CONFIGURATION
# =============================================================================

DATABASES = {
   
    "plomid": {
        "name": "PLOMID",
        "dsn": "postgresql://plomid:plomid@127.0.0.1:6000/plomid",
    },
     "postgresql": {
        "name": "PostgreSQL",
        "dsn": "postgresql://127.0.0.1:5432/postgres",
    }
}


SCHEMA = "benchmark_10m"
EVENTS_TABLE = "events"
CUSTOMERS_TABLE = "benchmark_customers"

# DEFAULT_ROWS = 10_000_000
DEFAULT_ROWS = 10000

DEFAULT_BATCH_SIZE = 10_000

# Dimension-table seed size. This used to be a hidden 100k-row workload
# regardless of --rows; it is now an explicit, overridable knob so a small
# run (e.g. --rows 1 --customers 1) actually stays small.
DEFAULT_CUSTOMERS = 100_000

DEFAULT_QUERY_COUNT = 100
DEFAULT_CONCURRENCY = 8
DEFAULT_CONCURRENT_OPERATIONS = 200

RANDOM_SEED = 42

RESULTS_DIR = "benchmark_results"


# =============================================================================
# SQL
# =============================================================================

DROP_SCHEMA_SQL = f"""
DROP SCHEMA IF EXISTS {SCHEMA} CASCADE
"""


CREATE_SCHEMA_SQL = f"""
CREATE SCHEMA {SCHEMA}
"""


CREATE_EVENTS_SQL = f"""
CREATE TABLE {SCHEMA}.{EVENTS_TABLE} (
    id              BIGINT PRIMARY KEY,
    unique_key      VARCHAR(64) NOT NULL UNIQUE,
    customer_id     BIGINT NOT NULL,
    category_id     INTEGER NOT NULL,
    region_id       INTEGER NOT NULL,
    event_type      INTEGER NOT NULL,

    amount          NUMERIC(14,2) NOT NULL,
    score           DOUBLE PRECISION NOT NULL,
    quantity        INTEGER NOT NULL,

    status          VARCHAR(32) NOT NULL,
    email           VARCHAR(128) NOT NULL,

    region          VARCHAR(32) NOT NULL,

    event_date      DATE NOT NULL,
    created_at      TIMESTAMP NOT NULL,

    is_active       BOOLEAN NOT NULL,

    nullable_value  INTEGER,

    json_data       JSON,

    description     TEXT
)
"""


CREATE_CUSTOMERS_SQL = f"""
CREATE TABLE {SCHEMA}.{CUSTOMERS_TABLE} (
    customer_id     BIGINT PRIMARY KEY,
    customer_name   VARCHAR(128) NOT NULL,
    region          VARCHAR(32) NOT NULL
)
"""


INDEXES = [
    (
        "idx_events_customer_id",
        f"""
        CREATE INDEX idx_events_customer_id
        ON {SCHEMA}.{EVENTS_TABLE}(customer_id)
        """,
    ),
    (
        "idx_events_category_id",
        f"""
        CREATE INDEX idx_events_category_id
        ON {SCHEMA}.{EVENTS_TABLE}(category_id)
        """,
    ),
    (
        "idx_events_region_id",
        f"""
        CREATE INDEX idx_events_region_id
        ON {SCHEMA}.{EVENTS_TABLE}(region_id)
        """,
    ),
    (
        "idx_events_event_date",
        f"""
        CREATE INDEX idx_events_event_date
        ON {SCHEMA}.{EVENTS_TABLE}(event_date)
        """,
    ),
    (
        "idx_events_customer_date",
        f"""
        CREATE INDEX idx_events_customer_date
        ON {SCHEMA}.{EVENTS_TABLE}(customer_id, event_date)
        """,
    ),
]


# =============================================================================
# RESULT STRUCTURES
# =============================================================================

RESULTS = {
    "metadata": {},
    "databases": {},
    "comparison": {},
}


# =============================================================================
# UTILITY FUNCTIONS
# =============================================================================

def ensure_results_dir():
    os.makedirs(RESULTS_DIR, exist_ok=True)


def now_iso():
    return datetime.now(timezone.utc).isoformat()


def percentile(values, percentile_value):
    if not values:
        return 0.0

    ordered = sorted(values)

    position = (len(ordered) - 1) * (percentile_value / 100.0)

    lower = math.floor(position)
    upper = math.ceil(position)

    if lower == upper:
        return ordered[lower]

    return (
        ordered[lower]
        + (ordered[upper] - ordered[lower]) * (position - lower)
    )


def latency_statistics(values):
    if not values:
        return {
            "count": 0,
            "avg_ms": None,
            "min_ms": None,
            "max_ms": None,
            "p50_ms": None,
            "p95_ms": None,
            "p99_ms": None,
        }

    return {
        "count": len(values),
        "avg_ms": statistics.mean(values) * 1000,
        "min_ms": min(values) * 1000,
        "max_ms": max(values) * 1000,
        "p50_ms": percentile(values, 50) * 1000,
        "p95_ms": percentile(values, 95) * 1000,
        "p99_ms": percentile(values, 99) * 1000,
    }


def timed(fn):
    start = time.perf_counter()

    result = fn()

    elapsed = time.perf_counter() - start

    return result, elapsed


def safe_float(value):
    if value is None:
        return None

    return float(value)


# =============================================================================
# CONNECTION
# =============================================================================

def connect_database(config):

    start = time.perf_counter()

    conn = psycopg2.connect(
        config["dsn"],
        connect_timeout=30,
    )

    elapsed = time.perf_counter() - start

    conn.autocommit = False

    return conn, elapsed


# =============================================================================
# FEATURE RECORDING
# =============================================================================

def record_operation(db_results, name, status, elapsed=None, error=None, extra=None):

    operation = {
        "status": status,
        "elapsed_s": elapsed,
    }

    if error:
        operation["error"] = str(error)

    if extra:
        operation.update(extra)

    db_results["operations"][name] = operation


def run_optional_operation(db_results, name, fn):

    start = time.perf_counter()

    try:

        result = fn()

        elapsed = time.perf_counter() - start

        record_operation(
            db_results,
            name,
            "PASS",
            elapsed,
            extra={"result": result},
        )

        return True, result

    except Exception as exc:

        elapsed = time.perf_counter() - start

        record_operation(
            db_results,
            name,
            "UNSUPPORTED_OR_FAILED",
            elapsed,
            error=exc,
        )

        print(
            f"[{db_results['database']}] "
            f"{name}: FAILED/UNSUPPORTED: {exc}"
        )

        try:
            db_results["connection"].rollback()
        except Exception:
            pass

        return False, None


# =============================================================================
# SCHEMA
# =============================================================================

def create_schema(conn, db_results):

    start = time.perf_counter()

    with conn.cursor() as cur:

        cur.execute(DROP_SCHEMA_SQL)
        cur.execute(CREATE_SCHEMA_SQL)
        cur.execute(CREATE_EVENTS_SQL)
        cur.execute(CREATE_CUSTOMERS_SQL)

    conn.commit()

    elapsed = time.perf_counter() - start

    record_operation(
        db_results,
        "schema_creation",
        "PASS",
        elapsed,
    )

    return elapsed


# =============================================================================
# CUSTOMER DATA
# =============================================================================

def generate_customer_rows(count):

    rows = []

    regions = [
        "APAC",
        "US",
        "EU",
        "LATAM",
        "MEA",
        "CANADA",
        "AUSTRALIA",
        "JAPAN",
    ]

    for customer_id in range(1, count + 1):

        region = regions[(customer_id - 1) % len(regions)]

        rows.append(
            (
                customer_id,
                f"customer-{customer_id}",
                region,
            )
        )

    return rows


def insert_customers(conn, db_results, count=100_000):

    start = time.perf_counter()

    rows = generate_customer_rows(count)

    with conn.cursor() as cur:

        execute_values(
            cur,
            f"""
            INSERT INTO {SCHEMA}.{CUSTOMERS_TABLE}
            (
                customer_id,
                customer_name,
                region
            )
            VALUES %s
            """,
            rows,
            page_size=10_000,
        )

    conn.commit()

    elapsed = time.perf_counter() - start

    record_operation(
        db_results,
        "customer_dimension_insert",
        "PASS",
        elapsed,
        extra={
            "rows": count,
            "rows_per_second": count / elapsed,
        },
    )

    return elapsed


# =============================================================================
# DETERMINISTIC DATA GENERATOR
# =============================================================================

def generate_rows(start_id, count):

    rows = []

    base_date = datetime(2020, 1, 1)

    statuses = [
        "active",
        "pending",
        "completed",
        "cancelled",
        "failed",
    ]

    regions = [
        "APAC",
        "US",
        "EU",
        "LATAM",
        "MEA",
    ]

    for i in range(count):

        row_id = start_id + i

        customer_id = ((row_id - 1) % 100_000) + 1

        category_id = ((row_id - 1) % 100) + 1

        region_id = ((row_id - 1) % 50) + 1

        event_type = ((row_id - 1) % 20) + 1

        amount = ((row_id * 17) % 1_000_000) / 100.0

        score = ((row_id * 31) % 100_000) / 10_000.0

        quantity = ((row_id - 1) % 100) + 1

        status = statuses[(row_id - 1) % len(statuses)]

        region = regions[(row_id - 1) % len(regions)]

        email = f"user{row_id}@example.com"

        event_date = (
            datetime(2016, 1, 1)
            + timedelta(days=(row_id % 3650))
        ).date()

        created_at = base_date + timedelta(
            seconds=row_id % (365 * 24 * 60 * 60)
        )

        is_active = (row_id % 2 == 0)

        nullable_value = (
            None
            if row_id % 10 == 0
            else (row_id % 10_000)
        )

        json_payload = {
            "customer": {
                "id": customer_id,
                "tier": [
                    "free",
                    "pro",
                    "business",
                    "enterprise",
                ][row_id % 4],
            },
            "device": {
                "type": [
                    "mobile",
                    "desktop",
                    "tablet",
                    "server",
                ][row_id % 4],
                "os": [
                    "linux",
                    "macos",
                    "windows",
                    "android",
                ][row_id % 4],
            },
            "transaction": {
                "currency": [
                    "USD",
                    "EUR",
                    "INR",
                    "JPY",
                ][row_id % 4],
                "amount": amount,
            },
            "metadata": {
                "source": "api",
                "campaign": f"campaign-{row_id % 100}",
            },
            "tags": [
                "data",
                "sql",
                "distributed",
            ],
        }

        rows.append(
            (
                row_id,
                f"UK-{row_id:012d}",
                customer_id,
                category_id,
                region_id,
                event_type,
                amount,
                score,
                quantity,
                status,
                email,
                region,
                event_date,
                created_at,
                is_active,
                nullable_value,
                json.dumps(json_payload),
                f"event-{row_id % 1000}",
            )
        )

    return rows


# =============================================================================
# 10M INSERT
# =============================================================================

def insert_events(conn, db_results, rows_count, batch_size):

    print(
        f"[{db_results['database']}] "
        f"Starting {rows_count:,}-row insert"
    )

    start_global = time.perf_counter()

    total_inserted = 0

    batch_times = []

    with conn.cursor() as cur:

        next_id = 1

        while total_inserted < rows_count:

            current_batch = min(
                batch_size,
                rows_count - total_inserted,
            )

            rows = generate_rows(
                next_id,
                current_batch,
            )

            start = time.perf_counter()

            execute_values(
                cur,
                f"""
                INSERT INTO {SCHEMA}.{EVENTS_TABLE}
                (
                    id,
                    unique_key,
                    customer_id,
                    category_id,
                    region_id,
                    event_type,
                    amount,
                    score,
                    quantity,
                    status,
                    email,
                    region,
                    event_date,
                    created_at,
                    is_active,
                    nullable_value,
                    json_data,
                    description
                )
                VALUES %s
                """,
                rows,
                page_size=batch_size,
            )

            conn.commit()

            elapsed = time.perf_counter() - start

            batch_times.append(elapsed)

            total_inserted += current_batch

            next_id += current_batch

            if (
                total_inserted % max(batch_size * 10, 100_000) == 0
                or total_inserted == rows_count
            ):

                global_elapsed = time.perf_counter() - start_global

                rate = total_inserted / global_elapsed

                print(
                    f"[{db_results['database']}] "
                    f"{total_inserted:,}/{rows_count:,} "
                    f"({total_inserted / rows_count * 100:.1f}%) "
                    f"{rate:,.0f} rows/sec"
                )

    total_time = time.perf_counter() - start_global

    rows_per_second = rows_count / total_time

    result = {
        "rows": rows_count,
        "elapsed_s": total_time,
        "rows_per_second": rows_per_second,
        "batch_avg_s": (
            statistics.mean(batch_times)
            if batch_times
            else 0
        ),
    }

    record_operation(
        db_results,
        "10m_insert",
        "PASS",
        total_time,
        extra=result,
    )

    return result


# =============================================================================
# INDEX CREATION
# =============================================================================

def create_indexes(conn, db_results):

    results = {}

    for index_name, sql in INDEXES:

        print(
            f"[{db_results['database']}] "
            f"Creating {index_name}"
        )

        start = time.perf_counter()

        try:

            with conn.cursor() as cur:

                cur.execute(sql)

            conn.commit()

            elapsed = time.perf_counter() - start

            results[index_name] = {
                "status": "PASS",
                "elapsed_s": elapsed,
            }

        except Exception as exc:

            conn.rollback()

            elapsed = time.perf_counter() - start

            results[index_name] = {
                "status": "FAILED",
                "elapsed_s": elapsed,
                "error": str(exc),
            }

    db_results["index_creation"] = results

    return results


# =============================================================================
# ANALYZE / STATISTICS
# =============================================================================

def analyze_database(conn, db_results):

    def operation():

        with conn.cursor() as cur:

            cur.execute(
                f"""
                ANALYZE {SCHEMA}.{EVENTS_TABLE}
                """
            )

        conn.commit()

        return True

    return run_optional_operation(
        db_results,
        "analyze",
        operation,
    )


# =============================================================================
# BASIC QUERIES
# =============================================================================

def execute_query(conn, sql, params=None, fetch=True):

    with conn.cursor() as cur:

        cur.execute(sql, params)

        if fetch:
            return cur.fetchall()

        return None


def benchmark_single_queries(
    conn,
    db_results,
    query_count,
):

    print(
        f"[{db_results['database']}] "
        f"Running query benchmark"
    )

    queries = {

        "pk_lookup": (
            f"""
            SELECT *
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE id = %s
            """,
            lambda i: (i,),
        ),

        "unique_key_lookup": (
            f"""
            SELECT *
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE unique_key = %s
            """,
            lambda i: (f"UK-{i:012d}",),
        ),

        "customer_lookup": (
            f"""
            SELECT id, customer_id, amount
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE customer_id = %s
            LIMIT 100
            """,
            lambda i: (((i - 1) % 100_000) + 1,),
        ),

        "category_lookup": (
            f"""
            SELECT id, category_id, amount
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE category_id = %s
            LIMIT 100
            """,
            lambda i: (((i - 1) % 100) + 1,),
        ),

        "region_lookup": (
            f"""
            SELECT id, region_id, amount
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE region_id = %s
            LIMIT 100
            """,
            lambda i: (((i - 1) % 50) + 1,),
        ),

        "composite_lookup": (
            f"""
            SELECT id, customer_id, event_date, amount
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE customer_id = %s
              AND event_date >= %s
            LIMIT 100
            """,
            lambda i: (
                ((i - 1) % 100_000) + 1,
                datetime(2020, 1, 1).date(),
            ),
        ),

        "range_scan": (
            f"""
            SELECT id, customer_id, amount
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE id BETWEEN %s AND %s
            """,
            lambda i: (
                i,
                i + 999,
            ),
        ),

        "date_range": (
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE event_date BETWEEN %s AND %s
            """,
            lambda i: (
                datetime(2019, 1, 1).date(),
                datetime(2020, 1, 1).date(),
            ),
        ),

        "json_customer_id": (
            f"""
            SELECT id, json_data
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE json_data->'customer'->>'id' = %s
            LIMIT 100
            """,
            lambda i: (
                str(((i - 1) % DEFAULT_QUERY_COUNT) + 1),
            ),
        ),

        "json_tier": (
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE json_data->'customer'->>'tier' = %s
            """,
            lambda i: (
                [
                    "free",
                    "pro",
                    "business",
                    "enterprise",
                ][i % 4],
            ),
        ),

        "json_device": (
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE json_data->'device'->>'os' = %s
            """,
            lambda i: (
                [
                    "linux",
                    "macos",
                    "windows",
                    "android",
                ][i % 4],
            ),
        ),

        "count": (
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{EVENTS_TABLE}
            """,
            lambda i: (),
        ),

        "aggregation": (
            f"""
            SELECT
                SUM(amount),
                AVG(amount),
                MIN(amount),
                MAX(amount),
                SUM(quantity)
            FROM {SCHEMA}.{EVENTS_TABLE}
            """,
            lambda i: (),
        ),

        "group_by_region": (
            f"""
            SELECT
                region_id,
                COUNT(*),
                SUM(amount),
                AVG(score)
            FROM {SCHEMA}.{EVENTS_TABLE}
            GROUP BY region_id
            ORDER BY region_id
            """,
            lambda i: (),
        ),

        "group_by_status": (
            f"""
            SELECT
                status,
                COUNT(*),
                AVG(amount)
            FROM {SCHEMA}.{EVENTS_TABLE}
            GROUP BY status
            ORDER BY status
            """,
            lambda i: (),
        ),

        "distinct_customer": (
            f"""
            SELECT DISTINCT customer_id
            FROM {SCHEMA}.{EVENTS_TABLE}
            ORDER BY customer_id
            LIMIT 1000
            """,
            lambda i: (),
        ),

        "order_by_amount": (
            f"""
            SELECT id, amount
            FROM {SCHEMA}.{EVENTS_TABLE}
            ORDER BY amount DESC
            LIMIT 100
            """,
            lambda i: (),
        ),

        "order_by_date": (
            f"""
            SELECT id, event_date
            FROM {SCHEMA}.{EVENTS_TABLE}
            ORDER BY event_date DESC
            LIMIT 100
            """,
            lambda i: (),
        ),

        "null_test": (
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE nullable_value IS NULL
            """,
            lambda i: (),
        ),

        "boolean_test": (
            f"""
            SELECT COUNT(*)
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE is_active = TRUE
            """,
            lambda i: (),
        ),

        "join": (
            f"""
            SELECT
                e.id,
                e.amount,
                c.customer_name,
                c.region
            FROM {SCHEMA}.{EVENTS_TABLE} e
            JOIN {SCHEMA}.{CUSTOMERS_TABLE} c
              ON e.customer_id = c.customer_id
            WHERE e.id = %s
            """,
            lambda i: (i,),
        ),
    }

    all_results = {}

    for name, (sql, parameter_generator) in queries.items():

        latencies = []

        errors = []

        for i in range(1, query_count + 1):

            params = parameter_generator(i)

            start = time.perf_counter()

            try:

                with conn.cursor() as cur:

                    cur.execute(sql, params)

                    cur.fetchall()

                elapsed = time.perf_counter() - start

                latencies.append(elapsed)

            except Exception as exc:

                elapsed = time.perf_counter() - start

                errors.append(str(exc))

                try:
                    conn.rollback()
                except Exception:
                    pass

                break

        stats = latency_statistics(latencies)

        if errors:

            all_results[name] = {
                "status": "UNSUPPORTED_OR_FAILED",
                "errors": errors[:5],
                **stats,
            }

        else:

            all_results[name] = {
                "status": "PASS",
                **stats,
                "queries_per_second": (
                    len(latencies) / sum(latencies)
                    if latencies and sum(latencies) > 0
                    else 0
                ),
            }

        print(
            f"[{db_results['database']}] "
            f"{name}: "
            f"{all_results[name]['status']} "
            f"p50="
            f"{all_results[name].get('p50_ms')}"
            f"ms"
        )

    db_results["queries"] = all_results

    return all_results


# =============================================================================
# UPDATE BENCHMARK
# =============================================================================

def benchmark_update(conn, db_results):

    def operation():

        start = time.perf_counter()

        with conn.cursor() as cur:

            cur.execute(
                f"""
                UPDATE {SCHEMA}.{EVENTS_TABLE}
                SET score = score + 0.001
                WHERE id BETWEEN 1 AND 100000
                """
            )

            affected = cur.rowcount

        conn.commit()

        elapsed = time.perf_counter() - start

        return {
            "affected_rows": affected,
            "rows_per_second": affected / elapsed,
            "elapsed_s": elapsed,
        }

    return run_optional_operation(
        db_results,
        "update_100k",
        operation,
    )


# =============================================================================
# DELETE BENCHMARK
# =============================================================================

def benchmark_delete(conn, db_results):

    def operation():

        start = time.perf_counter()

        with conn.cursor() as cur:

            cur.execute(
                f"""
                DELETE FROM {SCHEMA}.{EVENTS_TABLE}
                WHERE id > 909000000
                  AND id <= 100000000
                """
            )

            affected = cur.rowcount

        conn.commit()

        elapsed = time.perf_counter() - start

        return {
            "affected_rows": affected,
            "rows_per_second": affected / elapsed,
            "elapsed_s": elapsed,
        }

    return run_optional_operation(
        db_results,
        "delete_100k",
        operation,
    )


# =============================================================================
# TRANSACTION COMMIT
# =============================================================================

def benchmark_transaction_commit(conn, db_results):

    def operation():

        start = time.perf_counter()

        with conn.cursor() as cur:

            for i in range(1, 1001):

                cur.execute(
                    f"""
                    UPDATE {SCHEMA}.{EVENTS_TABLE}
                    SET score = score + 0.0001
                    WHERE id = %s
                    """,
                    (i,),
                )

        conn.commit()

        elapsed = time.perf_counter() - start

        return {
            "transactions": 1,
            "statements": 1000,
            "elapsed_s": elapsed,
        }

    return run_optional_operation(
        db_results,
        "transaction_commit",
        operation,
    )


# =============================================================================
# TRANSACTION ROLLBACK
# =============================================================================

def benchmark_transaction_rollback(conn, db_results):

    def operation():

        original_value = None

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT score
                FROM {SCHEMA}.{EVENTS_TABLE}
                WHERE id = 1
                """
            )

            original_value = cur.fetchone()[0]

            cur.execute(
                f"""
                UPDATE {SCHEMA}.{EVENTS_TABLE}
                SET score = score + 999999
                WHERE id = 1
                """
            )

        conn.rollback()

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT score
                FROM {SCHEMA}.{EVENTS_TABLE}
                WHERE id = 1
                """
            )

            after_rollback = cur.fetchone()[0]

        if original_value != after_rollback:

            raise RuntimeError(
                "Rollback verification failed"
            )

        return {
            "rollback_verified": True,
        }

    return run_optional_operation(
        db_results,
        "transaction_rollback",
        operation,
    )


# =============================================================================
# EXPLAIN
# =============================================================================

def benchmark_explain(conn, db_results):

    explain_queries = {

        "explain_pk":
            f"""
            EXPLAIN
            SELECT *
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE id = 5000000
            """,

        "explain_customer":
            f"""
            EXPLAIN
            SELECT *
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE customer_id = 50000
            """,

        "explain_date":
            f"""
            EXPLAIN
            SELECT *
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE event_date >= DATE '2020-01-01'
            """,

        "explain_json":
            f"""
            EXPLAIN
            SELECT *
            FROM {SCHEMA}.{EVENTS_TABLE}
            WHERE json_data->'customer'->>'tier' = 'enterprise'
            """,

        "explain_join":
            f"""
            EXPLAIN
            SELECT
                e.id,
                c.customer_name
            FROM {SCHEMA}.{EVENTS_TABLE} e
            JOIN {SCHEMA}.{CUSTOMERS_TABLE} c
              ON e.customer_id = c.customer_id
            WHERE e.id = 5000000
            """,
    }

    results = {}

    for name, sql in explain_queries.items():

        try:

            start = time.perf_counter()

            with conn.cursor() as cur:

                cur.execute(sql)

                rows = cur.fetchall()

            elapsed = time.perf_counter() - start

            results[name] = {
                "status": "PASS",
                "elapsed_s": elapsed,
                "plan": [
                    row[0]
                    for row in rows
                ],
            }

        except Exception as exc:

            conn.rollback()

            results[name] = {
                "status": "UNSUPPORTED_OR_FAILED",
                "error": str(exc),
            }

    db_results["explain"] = results

    return results


# =============================================================================
# CONCURRENT READS
# =============================================================================

def concurrent_read_worker(
    dsn,
    operation_count,
    seed,
):

    random.seed(seed)

    conn = None

    latencies = []

    errors = []

    try:

        conn = psycopg2.connect(
            dsn,
            connect_timeout=30,
        )

        conn.autocommit = True

        for _ in range(operation_count):

            row_id = random.randint(
                1,
                9_900_000,
            )

            start = time.perf_counter()

            try:

                with conn.cursor() as cur:

                    cur.execute(
                        f"""
                        SELECT
                            id,
                            customer_id,
                            amount,
                            status
                        FROM {SCHEMA}.{EVENTS_TABLE}
                        WHERE id = %s
                        """,
                        (row_id,),
                    )

                    cur.fetchone()

                latencies.append(
                    time.perf_counter() - start
                )

            except Exception as exc:

                errors.append(str(exc))

                try:
                    conn.rollback()
                except Exception:
                    pass

    finally:

        if conn:

            conn.close()

    return latencies, errors


def benchmark_concurrent_reads(
    dsn,
    db_results,
    concurrency,
    operations,
):

    start = time.perf_counter()

    latencies = []

    errors = []

    operations_per_worker = max(
        1,
        operations // concurrency,
    )

    with ThreadPoolExecutor(
        max_workers=concurrency
    ) as executor:

        futures = []

        for worker_id in range(concurrency):

            futures.append(
                executor.submit(
                    concurrent_read_worker,
                    dsn,
                    operations_per_worker,
                    RANDOM_SEED + worker_id,
                )
            )

        for future in as_completed(futures):

            worker_latencies, worker_errors = future.result()

            latencies.extend(worker_latencies)

            errors.extend(worker_errors)

    elapsed = time.perf_counter() - start

    stats = latency_statistics(latencies)

    result = {
        "status": (
            "PASS"
            if not errors
            else "PARTIAL_FAILURE"
        ),
        "concurrency": concurrency,
        "operations": len(latencies),
        "elapsed_s": elapsed,
        "operations_per_second": (
            len(latencies) / elapsed
            if elapsed > 0
            else 0
        ),
        "errors": errors[:10],
        **stats,
    }

    db_results["concurrent_reads"] = result

    return result


# =============================================================================
# CONCURRENT WRITE
# =============================================================================

def concurrent_write_worker(
    dsn,
    start_id,
    operation_count,
):

    conn = None

    latencies = []

    errors = []

    try:

        conn = psycopg2.connect(
            dsn,
            connect_timeout=30,
        )

        conn.autocommit = True

        for i in range(operation_count):

            row_id = start_id + i

            start = time.perf_counter()

            try:

                with conn.cursor() as cur:

                    cur.execute(
                        f"""
                        UPDATE {SCHEMA}.{EVENTS_TABLE}
                        SET score = score + 0.000001
                        WHERE id = %s
                        """,
                        (row_id,),
                    )

                latencies.append(
                    time.perf_counter() - start
                )

            except Exception as exc:

                errors.append(str(exc))

                try:
                    conn.rollback()
                except Exception:
                    pass

    finally:

        if conn:

            conn.close()

    return latencies, errors


def benchmark_concurrent_writes(
    dsn,
    db_results,
    concurrency,
    operations,
):

    start = time.perf_counter()

    latencies = []

    errors = []

    operations_per_worker = max(
        1,
        operations // concurrency,
    )

    # Use rows from 8,000,000 onward so the concurrent
    # write benchmark doesn't overlap the earlier DELETE range.

    base_id = 8_000_000

    with ThreadPoolExecutor(
        max_workers=concurrency
    ) as executor:

        futures = []

        for worker_id in range(concurrency):

            worker_start = (
                base_id
                + worker_id * operations_per_worker
            )

            futures.append(
                executor.submit(
                    concurrent_write_worker,
                    dsn,
                    worker_start,
                    operations_per_worker,
                )
            )

        for future in as_completed(futures):

            worker_latencies, worker_errors = future.result()

            latencies.extend(worker_latencies)

            errors.extend(worker_errors)

    elapsed = time.perf_counter() - start

    stats = latency_statistics(latencies)

    result = {
        "status": (
            "PASS"
            if not errors
            else "PARTIAL_FAILURE"
        ),
        "concurrency": concurrency,
        "operations": len(latencies),
        "elapsed_s": elapsed,
        "operations_per_second": (
            len(latencies) / elapsed
            if elapsed > 0
            else 0
        ),
        "errors": errors[:10],
        **stats,
    }

    db_results["concurrent_writes"] = result

    return result


# =============================================================================
# TABLE / DATABASE SIZE
# =============================================================================

def collect_size(conn, db_results):

    result = {}

    queries = {
        "events_relation_size":
            f"""
            SELECT pg_total_relation_size(
                '{SCHEMA}.{EVENTS_TABLE}'
            )
            """,

        "events_table_size":
            f"""
            SELECT pg_relation_size(
                '{SCHEMA}.{EVENTS_TABLE}'
            )
            """,

        "events_indexes_size":
            f"""
            SELECT pg_indexes_size(
                '{SCHEMA}.{EVENTS_TABLE}'
            )
            """,
    }

    for name, sql in queries.items():

        try:

            with conn.cursor() as cur:

                cur.execute(sql)

                value = cur.fetchone()[0]

            result[name] = {
                "status": "PASS",
                "bytes": value,
                "mb": value / (1024 * 1024),
                "gb": value / (1024 * 1024 * 1024),
            }

        except Exception as exc:

            conn.rollback()

            result[name] = {
                "status": "UNSUPPORTED_OR_FAILED",
                "error": str(exc),
            }

    db_results["size"] = result

    return result


# =============================================================================
# ROW COUNT VERIFICATION
# =============================================================================

def verify_row_count(conn, db_results):

    try:

        with conn.cursor() as cur:

            cur.execute(
                f"""
                SELECT COUNT(*)
                FROM {SCHEMA}.{EVENTS_TABLE}
                """
            )

            count = cur.fetchone()[0]

        result = {
            "status": "PASS",
            "rows": count,
        }

    except Exception as exc:

        conn.rollback()

        result = {
            "status": "FAILED",
            "error": str(exc),
        }

    db_results["row_count"] = result

    return result


# =============================================================================
# CLEANUP
# =============================================================================

def cleanup_database(conn, db_results):

    print(
        f"[{db_results['database']}] "
        f"Cleaning benchmark schema"
    )

    start = time.perf_counter()

    try:

        conn.rollback()

        with conn.cursor() as cur:

            cur.execute(DROP_SCHEMA_SQL)

        conn.commit()

        elapsed = time.perf_counter() - start

        # Verify schema is gone.

        verification = False

        with conn.cursor() as cur:

            cur.execute(
                """
                SELECT EXISTS (
                    SELECT 1
                    FROM information_schema.schemata
                    WHERE schema_name = %s
                )
                """,
                (SCHEMA,),
            )

            exists = cur.fetchone()[0]

            verification = not exists

        db_results["cleanup"] = {
            "status": (
                "PASS"
                if verification
                else "FAILED"
            ),
            "elapsed_s": elapsed,
            "schema_removed": verification,
        }

        return verification

    except Exception as exc:

        try:
            conn.rollback()
        except Exception:
            pass

        db_results["cleanup"] = {
            "status": "FAILED",
            "error": str(exc),
        }

        return False


# =============================================================================
# DATABASE BENCHMARK
# =============================================================================

def benchmark_database(
    key,
    rows,
    batch_size,
    query_count,
    concurrency,
    concurrent_operations,
    customers,
):

    config = DATABASES[key]

    db_results = {
        "database": config["name"],
        "dsn": config["dsn"],
        "operations": {},
        "index_creation": {},
        "queries": {},
        "explain": {},
        "size": {},
    }

    conn = None

    try:

        print()
        print("=" * 90)
        print(
            f"BENCHMARKING {config['name']}"
        )
        print("=" * 90)

        conn, connection_time = connect_database(
            config
        )

        db_results["connection"] = {
            "status": "PASS",
            "elapsed_s": connection_time,
        }

        db_results["connection_object"] = True

        create_schema(
            conn,
            db_results,
        )

        insert_customers(
            conn,
            db_results,
            count=customers,
        )

        insert_events(
            conn,
            db_results,
            rows,
            batch_size,
        )

        verify_row_count(
            conn,
            db_results,
        )

        create_indexes(
            conn,
            db_results,
        )

        analyze_database(
            conn,
            db_results,
        )

        benchmark_single_queries(
            conn,
            db_results,
            query_count,
        )

        benchmark_update(
            conn,
            db_results,
        )

        benchmark_transaction_commit(
            conn,
            db_results,
        )

        benchmark_transaction_rollback(
            conn,
            db_results,
        )

        benchmark_explain(
            conn,
            db_results,
        )

        benchmark_concurrent_reads(
            config["dsn"],
            db_results,
            concurrency,
            concurrent_operations,
        )

        benchmark_concurrent_writes(
            config["dsn"],
            db_results,
            concurrency,
            concurrent_operations,
        )

        collect_size(
            conn,
            db_results,
        )

        # DELETE is intentionally last among destructive DML tests.
        benchmark_delete(
            conn,
            db_results,
        )

        # Verify after DELETE.
        verify_row_count(
            conn,
            db_results,
        )

    except Exception as exc:

        print()
        print(
            f"[{config['name']}] "
            f"FATAL BENCHMARK ERROR:"
        )

        traceback.print_exc()

        db_results["fatal_error"] = {
            "error": str(exc),
            "traceback": traceback.format_exc(),
        }

        if conn:

            try:
                conn.rollback()
            except Exception:
                pass

    finally:

        if conn:

            cleanup_database(
                conn,
                db_results,
            )

            try:
                conn.close()
            except Exception:
                pass

    # Do not expose connection object in JSON.
    db_results.pop("connection_object", None)

    return db_results


# =============================================================================
# COMPARISON
# =============================================================================

def compare_latency(
    postgres_result,
    plomid_result,
):

    if (
        postgres_result is None
        or plomid_result is None
    ):
        return None

    if (
        postgres_result == 0
        or plomid_result == 0
    ):
        return None

    return {
        "postgresql_ms": postgres_result,
        "plomid_ms": plomid_result,
        "plomid_vs_postgresql_ratio": (
            plomid_result / postgres_result
        ),
        "difference_percent": (
            (
                plomid_result
                - postgres_result
            )
            / postgres_result
            * 100
        ),
    }


def build_comparison():

    postgres = RESULTS["databases"].get(
        "postgresql",
        {},
    )

    plomid = RESULTS["databases"].get(
        "plomid",
        {},
    )

    comparison = {
        "summary": {},
        "queries": {},
        "operations": {},
    }

    # -------------------------------------------------------------------------
    # INSERT
    # -------------------------------------------------------------------------

    pg_insert = (
        postgres
        .get("operations", {})
        .get("10m_insert", {})
    )

    plomid_insert = (
        plomid
        .get("operations", {})
        .get("10m_insert", {})
    )

    if (
        pg_insert.get("rows_per_second")
        and plomid_insert.get("rows_per_second")
    ):

        comparison["operations"]["10m_insert"] = {
            "postgresql_rows_per_second":
                pg_insert["rows_per_second"],

            "plomid_rows_per_second":
                plomid_insert["rows_per_second"],

            "plomid_vs_postgresql_ratio":
                (
                    pg_insert["rows_per_second"]
                    / plomid_insert["rows_per_second"]
                ),

            "difference_percent":
                (
                    (
                        plomid_insert["rows_per_second"]
                        - pg_insert["rows_per_second"]
                    )
                    / pg_insert["rows_per_second"]
                    * 100
                ),
        }

    # -------------------------------------------------------------------------
    # QUERY LATENCY
    # -------------------------------------------------------------------------

    pg_queries = postgres.get(
        "queries",
        {},
    )

    plomid_queries = plomid.get(
        "queries",
        {},
    )

    query_names = sorted(
        set(pg_queries)
        | set(plomid_queries)
    )

    for name in query_names:

        pg = pg_queries.get(name, {})
        pl = plomid_queries.get(name, {})

        comparison["queries"][name] = {
            "postgresql": pg,
            "plomid": pl,
        }

        if (
            pg.get("p50_ms") is not None
            and pl.get("p50_ms") is not None
        ):

            comparison["queries"][name][
                "p50_comparison"
            ] = compare_latency(
                pg["p50_ms"],
                pl["p50_ms"],
            )

        if (
            pg.get("p95_ms") is not None
            and pl.get("p95_ms") is not None
        ):

            comparison["queries"][name][
                "p95_comparison"
            ] = compare_latency(
                pg["p95_ms"],
                pl["p95_ms"],
            )

        if (
            pg.get("p99_ms") is not None
            and pl.get("p99_ms") is not None
        ):

            comparison["queries"][name][
                "p99_comparison"
            ] = compare_latency(
                pg["p99_ms"],
                pl["p99_ms"],
            )

    # -------------------------------------------------------------------------
    # CONCURRENT
    # -------------------------------------------------------------------------

    for name in [
        "concurrent_reads",
        "concurrent_writes",
    ]:

        pg = postgres.get(name, {})
        pl = plomid.get(name, {})

        if (
            pg.get("operations_per_second")
            and pl.get("operations_per_second")
        ):

            comparison["operations"][name] = {

                "postgresql_ops_per_second":
                    pg["operations_per_second"],

                "plomid_ops_per_second":
                    pl["operations_per_second"],

                "plomid_vs_postgresql_ratio":
                    (
                        pl["operations_per_second"]
                        / pg["operations_per_second"]
                    ),

                "difference_percent":
                    (
                        (
                            pl["operations_per_second"]
                            - pg["operations_per_second"]
                        )
                        / pg["operations_per_second"]
                        * 100
                    ),
            }

    RESULTS["comparison"] = comparison

    return comparison


# =============================================================================
# CSV
# =============================================================================

def write_comparison_csv():

    path = os.path.join(
        RESULTS_DIR,
        "comparison.csv",
    )

    rows = []

    comparison = RESULTS["comparison"]

    for name, data in comparison.get(
        "operations",
        {},
    ).items():

        if "postgresql_rows_per_second" in data:

            rows.append(
                {
                    "workload": name,
                    "metric": "rows/sec",
                    "postgresql":
                        data[
                            "postgresql_rows_per_second"
                        ],
                    "plomid":
                        data[
                            "plomid_rows_per_second"
                        ],
                    "plomid_vs_postgresql_ratio":
                        data[
                            "plomid_vs_postgresql_ratio"
                        ],
                    "difference_percent":
                        data[
                            "difference_percent"
                        ],
                }
            )

        elif "postgresql_ops_per_second" in data:

            rows.append(
                {
                    "workload": name,
                    "metric": "ops/sec",
                    "postgresql":
                        data[
                            "postgresql_ops_per_second"
                        ],
                    "plomid":
                        data[
                            "plomid_ops_per_second"
                        ],
                    "plomid_vs_postgresql_ratio":
                        data[
                            "plomid_vs_postgresql_ratio"
                        ],
                    "difference_percent":
                        data[
                            "difference_percent"
                        ],
                }
            )

    for name, data in comparison.get(
        "queries",
        {},
    ).items():

        for percentile_name in [
            "p50_comparison",
            "p95_comparison",
            "p99_comparison",
        ]:

            comparison_data = data.get(
                percentile_name
            )

            if not comparison_data:
                continue

            rows.append(
                {
                    "workload": name,
                    "metric":
                        percentile_name.replace(
                            "_comparison",
                            "",
                        ),
                    "postgresql":
                        comparison_data[
                            "postgresql_ms"
                        ],
                    "plomid":
                        comparison_data[
                            "plomid_ms"
                        ],
                    "plomid_vs_postgresql_ratio":
                        comparison_data[
                            "plomid_vs_postgresql_ratio"
                        ],
                    "difference_percent":
                        comparison_data[
                            "difference_percent"
                        ],
                }
            )

    fieldnames = [
        "workload",
        "metric",
        "postgresql",
        "plomid",
        "plomid_vs_postgresql_ratio",
        "difference_percent",
    ]

    with open(
        path,
        "w",
        newline="",
    ) as f:

        writer = csv.DictWriter(
            f,
            fieldnames=fieldnames,
        )

        writer.writeheader()

        writer.writerows(rows)

    return path


# =============================================================================
# MARKDOWN REPORT
# =============================================================================

def fmt(value):

    if value is None:
        return "N/A"

    if isinstance(value, float):

        if abs(value) >= 1000:
            return f"{value:,.2f}"

        return f"{value:.4f}"

    return str(value)


def write_markdown_report():

    path = os.path.join(
        RESULTS_DIR,
        "benchmark_report.md",
    )

    postgres = RESULTS["databases"].get(
        "postgresql",
        {},
    )

    plomid = RESULTS["databases"].get(
        "plomid",
        {},
    )

    comparison = RESULTS["comparison"]

    lines = []

    lines.append(
        "# PLOMID vs PostgreSQL — 10M Benchmark Report"
    )

    lines.append("")

    lines.append(
        f"Generated: `{now_iso()}`"
    )

    lines.append("")

    lines.append(
        "> This report contains measured benchmark results. "
        "It does not assume that either system is faster."
    )

    lines.append("")

    lines.append("## Benchmark Configuration")

    lines.append("")

    lines.append(
        f"- Rows: **{RESULTS['metadata']['rows']:,}**"
    )

    lines.append(
        f"- Batch size: **{RESULTS['metadata']['batch_size']:,}**"
    )

    lines.append(
        f"- Repeated queries: **{RESULTS['metadata']['query_count']:,}**"
    )

    lines.append(
        f"- Concurrent workers: **{RESULTS['metadata']['concurrency']}**"
    )

    lines.append(
        f"- Concurrent operations: "
        f"**{RESULTS['metadata']['concurrent_operations']:,}**"
    )

    lines.append("")

    lines.append("## Environment")

    lines.append("")

    lines.append(
        f"- OS: `{RESULTS['metadata']['platform']}`"
    )

    lines.append(
        f"- Python: `{RESULTS['metadata']['python']}`"
    )

    lines.append("")

    # -------------------------------------------------------------------------
    # Insert
    # -------------------------------------------------------------------------

    lines.append("## 10M Row Insert")

    lines.append("")

    pg = (
        postgres
        .get("operations", {})
        .get("10m_insert", {})
    )

    pl = (
        plomid
        .get("operations", {})
        .get("10m_insert", {})
    )

    lines.append(
        "| Metric | PostgreSQL | PLOMID |"
    )

    lines.append(
        "|---|---:|---:|"
    )

    lines.append(
        f"| Time | "
        f"{fmt(pg.get('elapsed_s'))} s | "
        f"{fmt(pl.get('elapsed_s'))} s |"
    )

    lines.append(
        f"| Rows/sec | "
        f"{fmt(pg.get('rows_per_second'))} | "
        f"{fmt(pl.get('rows_per_second'))} |"
    )

    lines.append("")

    # -------------------------------------------------------------------------
    # Queries
    # -------------------------------------------------------------------------

    lines.append("## Query Latency")

    lines.append("")

    lines.append(
        "| Query | PostgreSQL p50 | PLOMID p50 | "
        "PostgreSQL p95 | PLOMID p95 | "
        "PostgreSQL p99 | PLOMID p99 |"
    )

    lines.append(
        "|---|---:|---:|---:|---:|---:|---:|"
    )

    query_names = sorted(
        set(
            postgres.get("queries", {})
        )
        |
        set(
            plomid.get("queries", {})
        )
    )

    for name in query_names:

        pgq = postgres.get(
            "queries",
            {},
        ).get(name, {})

        plq = plomid.get(
            "queries",
            {},
        ).get(name, {})

        lines.append(
            f"| `{name}` | "
            f"{fmt(pgq.get('p50_ms'))} ms | "
            f"{fmt(plq.get('p50_ms'))} ms | "
            f"{fmt(pgq.get('p95_ms'))} ms | "
            f"{fmt(plq.get('p95_ms'))} ms | "
            f"{fmt(pgq.get('p99_ms'))} ms | "
            f"{fmt(plq.get('p99_ms'))} ms |"
        )

    lines.append("")

    # -------------------------------------------------------------------------
    # Concurrent
    # -------------------------------------------------------------------------

    lines.append("## Concurrent Workloads")

    lines.append("")

    lines.append(
        "| Workload | PostgreSQL ops/sec | "
        "PLOMID ops/sec |"
    )

    lines.append(
        "|---|---:|---:|"
    )

    for name in [
        "concurrent_reads",
        "concurrent_writes",
    ]:

        pgc = postgres.get(
            name,
            {},
        )

        plc = plomid.get(
            name,
            {},
        )

        lines.append(
            f"| `{name}` | "
            f"{fmt(pgc.get('operations_per_second'))} | "
            f"{fmt(plc.get('operations_per_second'))} |"
        )

    lines.append("")

    # -------------------------------------------------------------------------
    # Index creation
    # -------------------------------------------------------------------------

    lines.append("## Index Creation")

    lines.append("")

    lines.append(
        "| Index | PostgreSQL | PLOMID |"
    )

    lines.append(
        "|---|---:|---:|"
    )

    all_indexes = set(
        postgres.get(
            "index_creation",
            {},
        )
    ) | set(
        plomid.get(
            "index_creation",
            {},
        )
    )

    for index_name in sorted(all_indexes):

        pgidx = postgres.get(
            "index_creation",
            {},
        ).get(index_name, {})

        plidx = plomid.get(
            "index_creation",
            {},
        ).get(index_name, {})

        lines.append(
            f"| `{index_name}` | "
            f"{pgidx.get('status', 'N/A')} "
            f"{fmt(pgidx.get('elapsed_s'))} s | "
            f"{plidx.get('status', 'N/A')} "
            f"{fmt(plidx.get('elapsed_s'))} s |"
        )

    lines.append("")

    # -------------------------------------------------------------------------
    # Feature Support
    # -------------------------------------------------------------------------

    lines.append("## Feature / Operation Status")

    lines.append("")

    lines.append(
        "| Operation | PostgreSQL | PLOMID |"
    )

    lines.append(
        "|---|---|---|"
    )

    operation_names = sorted(
        set(
            postgres.get(
                "operations",
                {},
            )
        )
        |
        set(
            plomid.get(
                "operations",
                {},
            )
        )
    )

    for name in operation_names:

        pgo = postgres.get(
            "operations",
            {},
        ).get(name, {})

        plo = plomid.get(
            "operations",
            {},
        ).get(name, {})

        lines.append(
            f"| `{name}` | "
            f"{pgo.get('status', 'N/A')} | "
            f"{plo.get('status', 'N/A')} |"
        )

    lines.append("")

    # -------------------------------------------------------------------------
    # Row counts
    # -------------------------------------------------------------------------

    lines.append("## Data Verification")

    lines.append("")

    lines.append(
        "| Database | Initial/final count | Cleanup |"
    )

    lines.append(
        "|---|---:|---|"
    )

    lines.append(
        f"| PostgreSQL | "
        f"{fmt(postgres.get('row_count', {}).get('rows'))} | "
        f"{postgres.get('cleanup', {}).get('status', 'N/A')} |"
    )

    lines.append(
        f"| PLOMID | "
        f"{fmt(plomid.get('row_count', {}).get('rows'))} | "
        f"{plomid.get('cleanup', {}).get('status', 'N/A')} |"
    )

    lines.append("")

    # -------------------------------------------------------------------------
    # Size
    # -------------------------------------------------------------------------

    lines.append("## Storage Size")

    lines.append("")

    lines.append(
        "| Metric | PostgreSQL | PLOMID |"
    )

    lines.append(
        "|---|---:|---:|"
    )

    for metric in [
        "events_relation_size",
        "events_table_size",
        "events_indexes_size",
    ]:

        pgsize = postgres.get(
            "size",
            {},
        ).get(metric, {})

        plsize = plomid.get(
            "size",
            {},
        ).get(metric, {})

        lines.append(
            f"| `{metric}` | "
            f"{fmt(pgsize.get('mb'))} MB | "
            f"{fmt(plsize.get('mb'))} MB |"
        )

    lines.append("")

    # -------------------------------------------------------------------------
    # Explain
    # -------------------------------------------------------------------------

    lines.append("## EXPLAIN / Query Plans")

    lines.append("")

    for db_name, db in [
        ("PostgreSQL", postgres),
        ("PLOMID", plomid),
    ]:

        lines.append(
            f"### {db_name}"
        )

        lines.append("")

        explain = db.get(
            "explain",
            {},
        )

        for name, data in explain.items():

            lines.append(
                f"#### `{name}`"
            )

            lines.append("")

            if data.get("status") == "PASS":

                lines.append("```text")

                lines.extend(
                    data.get(
                        "plan",
                        [],
                    )
                )

                lines.append("```")

            else:

                lines.append(
                    f"Status: `{data.get('status')}`"
                )

                if data.get("error"):

                    lines.append(
                        f"Error: `{data['error']}`"
                    )

            lines.append("")

    # -------------------------------------------------------------------------
    # Methodology
    # -------------------------------------------------------------------------

    lines.append("## Methodology")

    lines.append("")

    lines.append(
        "Both systems were tested using the same deterministic dataset "
        "generator, schema, logical data distribution, indexes, SQL "
        "workloads, query counts, and client-side benchmark process."
    )

    lines.append("")

    lines.append(
        "The benchmark intentionally records unsupported SQL/features "
        "separately from performance measurements. An unsupported feature "
        "must not be interpreted as a zero-latency result."
    )

    lines.append("")

    lines.append(
        "The benchmark schema is dropped after testing."
    )

    lines.append("")

    lines.append(
        "This benchmark is a point-in-time measurement on the machine "
        "and software versions used for the run. It should not be treated "
        "as a universal performance claim."
    )

    lines.append("")

    with open(
        path,
        "w",
    ) as f:

        f.write(
            "\n".join(lines)
        )

    return path


# =============================================================================
# JSON REPORT
# =============================================================================

def write_json():

    path = os.path.join(
        RESULTS_DIR,
        "benchmark_results.json",
    )

    with open(
        path,
        "w",
    ) as f:

        json.dump(
            RESULTS,
            f,
            indent=2,
            default=str,
        )

    return path


# =============================================================================
# HUMAN SUMMARY
# =============================================================================

def print_summary():

    print()
    print("=" * 90)
    print("BENCHMARK COMPLETE")
    print("=" * 90)

    for key in [
        "postgresql",
        "plomid",
    ]:

        db = RESULTS["databases"].get(
            key,
            {},
        )

        name = db.get(
            "database",
            key,
        )

        insert = db.get(
            "operations",
            {},
        ).get(
            "10m_insert",
            {},
        )

        print()
        print(name)

        print(
            "  Insert time:",
            fmt(insert.get("elapsed_s")),
            "s",
        )

        print(
            "  Insert rows/sec:",
            fmt(insert.get("rows_per_second")),
        )

        print(
            "  Cleanup:",
            db.get(
                "cleanup",
                {},
            ).get(
                "status",
                "N/A",
            ),
        )

    print()
    print(
        "Results directory:",
        RESULTS_DIR,
    )

    print()
    print(
        "Files:"
    )

    print(
        "  benchmark_results.json"
    )

    print(
        "  comparison.csv"
    )

    print(
        "  benchmark_report.md"
    )

    print()


# =============================================================================
# MAIN
# =============================================================================

def parse_args():

    parser = argparse.ArgumentParser(
        description=(
            "PLOMID vs PostgreSQL 10M-row benchmark"
        )
    )

    parser.add_argument(
        "--rows",
        type=int,
        default=DEFAULT_ROWS,
        help=(
            "Number of event rows. "
            "Default: 10,000,000"
        ),
    )

    parser.add_argument(
        "--batch-size",
        type=int,
        default=DEFAULT_BATCH_SIZE,
        help=(
            "Insert batch size. "
            "Default: 10,000"
        ),
    )

    parser.add_argument(
        "--customers",
        type=int,
        default=DEFAULT_CUSTOMERS,
        help=(
            "Dimension-table customer rows seeded before the event "
            "insert. Default: 100,000"
        ),
    )

    parser.add_argument(
        "--queries",
        type=int,
        default=DEFAULT_QUERY_COUNT,
        help=(
            "Repeated query count per workload. "
            "Default: 100"
        ),
    )

    parser.add_argument(
        "--concurrency",
        type=int,
        default=DEFAULT_CONCURRENCY,
        help=(
            "Concurrent workers. "
            "Default: 8"
        ),
    )

    parser.add_argument(
        "--concurrent-operations",
        type=int,
        default=DEFAULT_CONCURRENT_OPERATIONS,
        help=(
            "Total concurrent operations. "
            "Default: 200"
        ),
    )

    parser.add_argument(
        "--only",
        choices=[
            "postgresql",
            "plomid",
            "both",
        ],
        default="both",
        help="Database(s) to benchmark.",
    )

    return parser.parse_args()


def main():

    args = parse_args()

    ensure_results_dir()

    RESULTS["metadata"] = {
        "started_at": now_iso(),
        "rows": args.rows,
        "customer_rows": args.customers,
        "batch_size": args.batch_size,
        "query_count": args.queries,
        "concurrency": args.concurrency,
        "concurrent_operations":
            args.concurrent_operations,
        "platform":
            platform.platform(),
        "python":
            platform.python_version(),
        "processor":
            platform.processor(),
        "machine":
            platform.machine(),
        "random_seed":
            RANDOM_SEED,
    }

    print()
    print("=" * 90)
    print("PLOMID vs PostgreSQL")
    print("10M SCALE BENCHMARK")
    print("=" * 90)

    print()
    print(
        f"Rows:              {args.rows:,}"
    )

    print(
        f"Batch size:        {args.batch_size:,}"
    )

    print(
        f"Customer rows:     {args.customers:,}"
    )

    print(
        f"Repeated queries:  {args.queries:,}"
    )

    print(
        f"Concurrency:       {args.concurrency}"
    )

    print(
        f"Concurrent ops:    {args.concurrent_operations:,}"
    )

    print()

    if args.only in (
        "postgresql",
        "both",
    ):

        RESULTS["databases"]["postgresql"] = (
            benchmark_database(
                "postgresql",
                args.rows,
                args.batch_size,
                args.queries,
                args.concurrency,
                args.concurrent_operations,
                args.customers,
            )
        )

    if args.only in (
        "plomid",
        "both",
    ):

        RESULTS["databases"]["plomid"] = (
            benchmark_database(
                "plomid",
                args.rows,
                args.batch_size,
                args.queries,
                args.concurrency,
                args.concurrent_operations,
                args.customers,
            )
        )

    RESULTS["metadata"]["finished_at"] = now_iso()

    if (
        "postgresql"
        in RESULTS["databases"]
        and "plomid"
        in RESULTS["databases"]
    ):

        build_comparison()

    write_json()

    write_comparison_csv()

    write_markdown_report()

    print_summary()


if __name__ == "__main__":
    main()