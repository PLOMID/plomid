\set ON_ERROR_STOP on

\echo '=================================================================='
\echo 'PLOMID TEMPORAL PRECISION DIFFERENTIAL TEST (PostgreSQL 17)'
\echo '=================================================================='

SET TIME ZONE 'UTC';

-- ==================================================================
-- 1. TIME precision 0..6
-- ==================================================================
\echo '1. TIME(p) precision 0..6'

SELECT TIME(0) '12:34:56.123456';
SELECT TIME(1) '12:34:56.123456';
SELECT TIME(2) '12:34:56.123456';
SELECT TIME(3) '12:34:56.123456';
SELECT TIME(4) '12:34:56.123456';
SELECT TIME(5) '12:34:56.123456';
SELECT TIME(6) '12:34:56.123456';

-- Boundary: rounding may carry through midnight.
SELECT TIME(0) '23:59:59.999999';
SELECT TIME(3) '23:59:59.999999';
SELECT TIME(6) '23:59:59.999999';

-- ==================================================================
-- 2. TIMESTAMP precision 0..6
-- ==================================================================
\echo '2. TIMESTAMP(p) precision 0..6'

SELECT TIMESTAMP(0) '2026-01-15 12:34:56.123456';
SELECT TIMESTAMP(1) '2026-01-15 12:34:56.123456';
SELECT TIMESTAMP(2) '2026-01-15 12:34:56.123456';
SELECT TIMESTAMP(3) '2026-01-15 12:34:56.123456';
SELECT TIMESTAMP(4) '2026-01-15 12:34:56.123456';
SELECT TIMESTAMP(5) '2026-01-15 12:34:56.123456';
SELECT TIMESTAMP(6) '2026-01-15 12:34:56.123456';

-- Boundary: rounding may carry into the next day.
SELECT TIMESTAMP(0) '2026-01-15 23:59:59.999999';
SELECT TIMESTAMP(3) '2026-01-15 23:59:59.999999';
SELECT TIMESTAMP(6) '2026-01-15 23:59:59.999999';

-- ==================================================================
-- 3. TIMESTAMPTZ precision 0..6
-- ==================================================================
\echo '3. TIMESTAMPTZ(p) precision 0..6'

SELECT TIMESTAMPTZ(0) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMPTZ(1) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMPTZ(2) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMPTZ(3) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMPTZ(4) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMPTZ(5) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMPTZ(6) '2026-01-15 12:34:56.123456+00';

-- ==================================================================
-- 4. Unparameterized types keep working
-- ==================================================================
\echo '4. Unparameterized temporal literals'

SELECT TIME '12:34:56';
SELECT TIMESTAMP '2026-01-15 12:34:56';
SELECT TIMESTAMPTZ '2026-01-15 12:34:56+00';
SELECT TIMESTAMP WITH TIME ZONE '2026-01-15 12:34:56+00';

-- ==================================================================
-- 5. Multiword TIMESTAMP WITH TIME ZONE forms
-- ==================================================================
\echo '5. TIMESTAMP WITH TIME ZONE(p) / TIMESTAMP(p) WITH TIME ZONE'

SELECT TIMESTAMP WITH TIME ZONE(3) '2026-01-15 12:34:56.123456+00';
SELECT TIMESTAMP(3) WITH TIME ZONE '2026-01-15 12:34:56.123456+00';

-- ==================================================================
-- 6. Casts preserve precision
-- ==================================================================
\echo '6. Parameterized casts'

SELECT '12:34:56.123456'::TIME(3);
SELECT '2026-01-15 12:34:56.123456'::TIMESTAMP(3);
SELECT '2026-01-15 12:34:56.123456+00'::TIMESTAMPTZ(3);

-- ==================================================================
-- 7. Comparison semantics
-- ==================================================================
\echo '7. Scalar temporal comparisons'

SELECT
    TIMESTAMP(3) '2026-01-15 12:34:56.123'
    =
    TIMESTAMP(6) '2026-01-15 12:34:56.123000';

SELECT
    TIMESTAMP(6) '2026-01-15 12:34:56.123456'
    >
    TIMESTAMP(6) '2026-01-15 12:34:56.123000';

-- ==================================================================
-- 8. Timezone independence of precision
-- ==================================================================
\echo '8. Precision under a non-UTC display zone'

SET TIME ZONE 'Asia/Kolkata';

SELECT TIMESTAMPTZ(3) '2026-01-15 12:34:56.123456+05:30';

SET TIME ZONE 'UTC';

-- ==================================================================
-- 9. TABLE STORAGE ROUND TRIP
-- ==================================================================
\echo '9. Table storage preserves declared precision'

DROP TABLE IF EXISTS temporal_precision_test;

CREATE TABLE temporal_precision_test (
    t0 TIME(0),
    t3 TIME(3),
    t6 TIME(6),

    ts0 TIMESTAMP(0),
    ts3 TIMESTAMP(3),
    ts6 TIMESTAMP(6),

    tz0 TIMESTAMPTZ(0),
    tz3 TIMESTAMPTZ(3),
    tz6 TIMESTAMPTZ(6)
);

INSERT INTO temporal_precision_test VALUES (
    '12:34:56.123456',
    '12:34:56.123456',
    '12:34:56.123456',

    '2026-01-15 12:34:56.123456',
    '2026-01-15 12:34:56.123456',
    '2026-01-15 12:34:56.123456',

    '2026-01-15 12:34:56.123456+00',
    '2026-01-15 12:34:56.123456+00',
    '2026-01-15 12:34:56.123456+00'
);

SELECT t0, t3, t6 FROM temporal_precision_test;
SELECT ts0, ts3, ts6 FROM temporal_precision_test;
SELECT tz0, tz3, tz6 FROM temporal_precision_test;
SELECT * FROM temporal_precision_test;

DROP TABLE IF EXISTS temporal_precision_test;

-- ==================================================================
-- 10. Invalid precision is rejected
-- ==================================================================
\echo '10. Invalid precision rejected'

SELECT TIME(7) '12:34:56';
SELECT TIMESTAMP(7) '2026-01-15 12:34:56';
SELECT TIMESTAMPTZ(7) '2026-01-15 12:34:56+00';
SELECT TIMESTAMP(100) '2026-01-15 12:34:56';

\echo '=================================================================='
\echo 'TEMPORAL PRECISION TEST COMPLETE'
\echo '=================================================================='