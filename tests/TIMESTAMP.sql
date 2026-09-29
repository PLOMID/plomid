
\set ON_ERROR_STOP on

\echo '========================================'
\echo 'PLOMID FRESH TIMESTAMP TEST'
\echo '========================================'

-- ============================================================
-- CLEAN START
-- ============================================================

DROP SCHEMA IF EXISTS plomid_timestamp_test CASCADE;

CREATE SCHEMA plomid_timestamp_test;

-- ============================================================
-- CREATE TEST TABLE
-- ============================================================

CREATE TABLE plomid_timestamp_test.customers (
    id BIGINT PRIMARY KEY,
    first_name VARCHAR(100) NOT NULL,
    last_name VARCHAR(100) NOT NULL,
    email VARCHAR(255) NOT NULL UNIQUE,
    country VARCHAR(100) NOT NULL,
    status VARCHAR(30) NOT NULL DEFAULT 'active',
    credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00,
    created_at TIMESTAMP NOT NULL
);

\echo ''
\echo 'TABLE CREATED'

-- ============================================================
-- 1. BASIC CURRENT_TIMESTAMP
-- ============================================================

\echo ''
\echo '1. Basic CURRENT_TIMESTAMP'

SELECT CURRENT_TIMESTAMP;

-- ============================================================
-- 2. CURRENT_TIMESTAMP TWICE
-- ============================================================

\echo ''
\echo '2. CURRENT_TIMESTAMP twice'

SELECT
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP;

-- ============================================================
-- 3. INSERT CURRENT_TIMESTAMP
-- ============================================================

\echo ''
\echo '3. INSERT using CURRENT_TIMESTAMP'

INSERT INTO plomid_timestamp_test.customers
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
    'Test',
    'Timestamp',
    'timestamp1@plomid.test',
    'India',
    'active',
    50000.00,
    CURRENT_TIMESTAMP
);

-- ============================================================
-- 4. VERIFY INSERT
-- ============================================================

\echo ''
\echo '4. Verify INSERT'

SELECT
    id,
    first_name,
    last_name,
    email,
    created_at
FROM plomid_timestamp_test.customers
WHERE id = 1;

-- ============================================================
-- 5. UPDATE CURRENT_TIMESTAMP
-- ============================================================

\echo ''
\echo '5. UPDATE using CURRENT_TIMESTAMP'

UPDATE plomid_timestamp_test.customers
SET created_at = CURRENT_TIMESTAMP
WHERE id = 1;

-- ============================================================
-- 6. VERIFY UPDATE
-- ============================================================

\echo ''
\echo '6. Verify UPDATE'

SELECT
    id,
    created_at
FROM plomid_timestamp_test.customers
WHERE id = 1;

-- ============================================================
-- 7. CAST()
-- ============================================================

\echo ''
\echo '7. CAST(CURRENT_TIMESTAMP AS TIMESTAMP)'

SELECT CAST(
    CURRENT_TIMESTAMP AS TIMESTAMP
);

-- ============================================================
-- 8. DOUBLE-COLON CAST
-- ============================================================

\echo ''
\echo '8. CURRENT_TIMESTAMP::TIMESTAMP'

SELECT CURRENT_TIMESTAMP::TIMESTAMP;

-- ============================================================
-- 9. COALESCE
-- ============================================================

\echo ''
\echo '9. CURRENT_TIMESTAMP inside COALESCE'

SELECT COALESCE(
    CURRENT_TIMESTAMP,
    '2026-01-01 00:00:00'
);

-- ============================================================
-- 10. CASE
-- ============================================================

\echo ''
\echo '10. CURRENT_TIMESTAMP inside CASE'

SELECT
    CASE
        WHEN CURRENT_TIMESTAMP IS NOT NULL
        THEN CURRENT_TIMESTAMP
    END;

-- ============================================================
-- 11. INSERT MULTIPLE ROWS
-- ============================================================

\echo ''
\echo '11. Multiple INSERT rows using CURRENT_TIMESTAMP'

INSERT INTO plomid_timestamp_test.customers
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
    2,
    'Test',
    'Two',
    'timestamp2@plomid.test',
    'India',
    'active',
    1000.00,
    CURRENT_TIMESTAMP
),
(
    3,
    'Test',
    'Three',
    'timestamp3@plomid.test',
    'USA',
    'active',
    2000.00,
    CURRENT_TIMESTAMP
),
(
    4,
    'Test',
    'Four',
    'timestamp4@plomid.test',
    'UK',
    'active',
    3000.00,
    CURRENT_TIMESTAMP
);

-- ============================================================
-- 12. VERIFY MULTIPLE INSERT
-- ============================================================

\echo ''
\echo '12. Verify multiple INSERT'

SELECT
    id,
    email,
    created_at
FROM plomid_timestamp_test.customers
ORDER BY id;

-- ============================================================
-- 13. INSERT + EXPRESSION
-- ============================================================

\echo ''
\echo '13. CURRENT_TIMESTAMP in expression'

INSERT INTO plomid_timestamp_test.customers
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
    5,
    'Test',
    'Expression',
    'timestamp5@plomid.test',
    'India',
    'active',
    5000.00,
    CAST(CURRENT_TIMESTAMP AS TIMESTAMP)
);

-- ============================================================
-- 14. COUNT
-- ============================================================

\echo ''
\echo '14. Final row count'

SELECT COUNT(*)
FROM plomid_timestamp_test.customers;

-- ============================================================
-- 15. CLEANUP
-- ============================================================

\echo ''
\echo '15. CLEANUP'

DROP SCHEMA plomid_timestamp_test CASCADE;

-- ============================================================
-- 16. VERIFY CLEANUP
-- ============================================================

\echo ''
\echo '16. Verify cleanup'

SELECT COUNT(*)
FROM information_schema.tables
WHERE table_schema = 'plomid_timestamp_test';

\echo ''
\echo '========================================'
\echo 'PLOMID TIMESTAMP TEST COMPLETE'
\echo 'DATABASE CLEAN'
\echo '========================================'

