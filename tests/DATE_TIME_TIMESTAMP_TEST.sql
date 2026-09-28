\set ON_ERROR_STOP on

\echo '============================================================'
\echo 'PLOMID DATE/TIME COMPATIBILITY TEST - PostgreSQL 17'
\echo '============================================================'

DROP SCHEMA IF EXISTS plomid_datetime_test CASCADE;
CREATE SCHEMA plomid_datetime_test;

SET TIME ZONE 'UTC';

-- ============================================================
-- 1. BASIC TYPE LITERALS
-- ============================================================

\echo ''
\echo '============================================================'
\echo '1. BASIC DATE/TIME TYPES'
\echo '============================================================'

SELECT DATE '2026-01-15';

SELECT TIME '12:34:56';

SELECT TIME '12:34:56.123456';

SELECT TIMESTAMP '2026-01-15 12:34:56';

SELECT TIMESTAMP '2026-01-15 12:34:56.123456';

SELECT TIMESTAMP WITH TIME ZONE
       '2026-01-15 12:34:56+00';

SELECT TIMESTAMPTZ
       '2026-01-15 12:34:56+05:30';

SELECT INTERVAL '1 year 2 months 3 days 04:05:06.123456';

-- ============================================================
-- 2. TYPE PRECISION
-- ============================================================

\echo ''
\echo '2. TYPE PRECISION'

SELECT TIME(0) '12:34:56.123456';

SELECT TIME(2) '12:34:56.123456';

SELECT TIME(3) '12:34:56.123456';

SELECT TIME(6) '12:34:56.123456';

SELECT TIMESTAMP(0) '2026-01-15 12:34:56.123456';

SELECT TIMESTAMP(2) '2026-01-15 12:34:56.123456';

SELECT TIMESTAMP(3) '2026-01-15 12:34:56.123456';

SELECT TIMESTAMP(6) '2026-01-15 12:34:56.123456';

SELECT TIMESTAMPTZ(0) '2026-01-15 12:34:56.123456+00';

SELECT TIMESTAMPTZ(3) '2026-01-15 12:34:56.123456+00';

SELECT TIMESTAMPTZ(6) '2026-01-15 12:34:56.123456+00';

-- ============================================================
-- 3. CURRENT DATE/TIME
-- ============================================================

\echo ''
\echo '3. CURRENT DATE/TIME'

SELECT CURRENT_DATE;

SELECT CURRENT_TIME;

SELECT CURRENT_TIME(0);

SELECT CURRENT_TIME(2);

SELECT CURRENT_TIME(6);

SELECT CURRENT_TIMESTAMP;

SELECT CURRENT_TIMESTAMP(0);

SELECT CURRENT_TIMESTAMP(2);

SELECT CURRENT_TIMESTAMP(3);

SELECT CURRENT_TIMESTAMP(6);

SELECT LOCALTIME;

SELECT LOCALTIME(0);

SELECT LOCALTIME(2);

SELECT LOCALTIME(6);

SELECT LOCALTIMESTAMP;

SELECT LOCALTIMESTAMP(0);

SELECT LOCALTIMESTAMP(2);

SELECT LOCALTIMESTAMP(6);

SELECT NOW();

SELECT TRANSACTION_TIMESTAMP();

SELECT STATEMENT_TIMESTAMP();

SELECT CLOCK_TIMESTAMP();

-- ============================================================
-- 4. CURRENT VALUES - MULTIPLE REFERENCES
-- ============================================================

\echo ''
\echo '4. CURRENT VALUES MULTIPLE REFERENCES'

SELECT
    CURRENT_DATE,
    CURRENT_DATE;

SELECT
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP;

SELECT
    NOW(),
    NOW();

SELECT
    TRANSACTION_TIMESTAMP(),
    STATEMENT_TIMESTAMP(),
    CLOCK_TIMESTAMP();

-- ============================================================
-- 5. CREATE TABLE WITH ALL DATE/TIME TYPES
-- ============================================================

\echo ''
\echo '5. DATE/TIME TABLE'

CREATE TABLE plomid_datetime_test.all_datetime_types (
    id BIGINT PRIMARY KEY,

    d DATE,

    t TIME,

    t_precision TIME(6),

    tz_time TIME WITH TIME ZONE,

    ts TIMESTAMP,

    ts_precision TIMESTAMP(6),

    tstz TIMESTAMP WITH TIME ZONE,

    tstz_precision TIMESTAMP(6) WITH TIME ZONE,

    duration INTERVAL,

    duration_year_month INTERVAL YEAR TO MONTH,

    duration_day_time INTERVAL DAY TO SECOND
);

-- ============================================================
-- 6. INSERT DATE/TIME VALUES
-- ============================================================

\echo ''
\echo '6. INSERT DATE/TIME VALUES'

INSERT INTO plomid_datetime_test.all_datetime_types
VALUES (
    1,
    DATE '2026-01-15',
    TIME '12:34:56',
    TIME '12:34:56.123456',
    TIME WITH TIME ZONE '12:34:56+05:30',
    TIMESTAMP '2026-01-15 12:34:56',
    TIMESTAMP '2026-01-15 12:34:56.123456',
    TIMESTAMPTZ '2026-01-15 12:34:56+00',
    TIMESTAMPTZ '2026-01-15 12:34:56.123456+05:30',
    INTERVAL '1 year 2 months 3 days 04:05:06',
    INTERVAL '1 year 2 months',
    INTERVAL '3 days 04:05:06'
);

SELECT *
FROM plomid_datetime_test.all_datetime_types
ORDER BY id;

-- ============================================================
-- 7. CASTING - DATE
-- ============================================================

\echo ''
\echo '7. DATE CASTS'

SELECT CAST('2026-01-15' AS DATE);

SELECT '2026-01-15'::DATE;

SELECT CAST(DATE '2026-01-15' AS TIMESTAMP);

SELECT DATE '2026-01-15'::TIMESTAMP;

SELECT CAST(DATE '2026-01-15' AS TIMESTAMPTZ);

SELECT DATE '2026-01-15'::TIMESTAMPTZ;

SELECT CAST(DATE '2026-01-15' AS TEXT);

SELECT DATE '2026-01-15'::TEXT;

-- ============================================================
-- 8. CASTING - TIME
-- ============================================================

\echo ''
\echo '8. TIME CASTS'

SELECT CAST('12:34:56' AS TIME);

SELECT '12:34:56'::TIME;

SELECT CAST('12:34:56.123456' AS TIME);

SELECT '12:34:56.123456'::TIME;

SELECT CAST(TIME '12:34:56' AS TEXT);

SELECT TIME '12:34:56'::TEXT;

SELECT CAST(TIME '12:34:56' AS TIMESTAMP);

SELECT TIME '12:34:56'::TIMESTAMP;

-- ============================================================
-- 9. CASTING - TIMESTAMP
-- ============================================================

\echo ''
\echo '9. TIMESTAMP CASTS'

SELECT CAST('2026-01-15 12:34:56' AS TIMESTAMP);

SELECT '2026-01-15 12:34:56'::TIMESTAMP;

SELECT CAST('2026-01-15 12:34:56.123456' AS TIMESTAMP);

SELECT '2026-01-15 12:34:56.123456'::TIMESTAMP;

SELECT CAST(TIMESTAMP '2026-01-15 12:34:56' AS DATE);

SELECT TIMESTAMP '2026-01-15 12:34:56'::DATE;

SELECT CAST(TIMESTAMP '2026-01-15 12:34:56' AS TIME);

SELECT TIMESTAMP '2026-01-15 12:34:56'::TIME;

SELECT CAST(TIMESTAMP '2026-01-15 12:34:56' AS TIMESTAMPTZ);

SELECT TIMESTAMP '2026-01-15 12:34:56'::TIMESTAMPTZ;

SELECT CAST(TIMESTAMP '2026-01-15 12:34:56' AS TEXT);

SELECT TIMESTAMP '2026-01-15 12:34:56'::TEXT;

-- ============================================================
-- 10. CASTING - TIMESTAMPTZ
-- ============================================================

\echo ''
\echo '10. TIMESTAMPTZ CASTS'

SELECT CAST('2026-01-15 12:34:56+00' AS TIMESTAMPTZ);

SELECT '2026-01-15 12:34:56+00'::TIMESTAMPTZ;

SELECT CAST('2026-01-15 12:34:56+05:30' AS TIMESTAMPTZ);

SELECT '2026-01-15 12:34:56+05:30'::TIMESTAMPTZ;

SELECT CAST(TIMESTAMPTZ '2026-01-15 12:34:56+00' AS TIMESTAMP);

SELECT TIMESTAMPTZ '2026-01-15 12:34:56+00'::TIMESTAMP;

SELECT CAST(TIMESTAMPTZ '2026-01-15 12:34:56+00' AS DATE);

SELECT TIMESTAMPTZ '2026-01-15 12:34:56+00'::DATE;

SELECT CAST(TIMESTAMPTZ '2026-01-15 12:34:56+00' AS TIME);

SELECT TIMESTAMPTZ '2026-01-15 12:34:56+00'::TIME;

-- ============================================================
-- 11. INTERVAL LITERALS
-- ============================================================

\echo ''
\echo '11. INTERVAL LITERALS'

SELECT INTERVAL '1 second';

SELECT INTERVAL '1 minute';

SELECT INTERVAL '1 hour';

SELECT INTERVAL '1 day';

SELECT INTERVAL '1 week';

SELECT INTERVAL '1 month';

SELECT INTERVAL '1 year';

SELECT INTERVAL '2 years 3 months';

SELECT INTERVAL '4 days 5 hours 6 minutes 7 seconds';

SELECT INTERVAL '1 year 2 months 3 days 04:05:06.123456';

SELECT INTERVAL '1 year 2 months' YEAR TO MONTH;

SELECT INTERVAL '3 days 04:05:06' DAY TO SECOND;

SELECT INTERVAL '04:05' HOUR TO MINUTE;

SELECT INTERVAL '04:05:06' HOUR TO SECOND;

SELECT INTERVAL '05:06' MINUTE TO SECOND;

-- ============================================================
-- 12. INTERVAL CASTING
-- ============================================================

\echo ''
\echo '12. INTERVAL CASTS'

SELECT CAST('1 day' AS INTERVAL);

SELECT '1 day'::INTERVAL;

SELECT CAST('2 years 3 months' AS INTERVAL);

SELECT '2 years 3 months'::INTERVAL;

SELECT CAST(INTERVAL '1 day' AS TEXT);

SELECT INTERVAL '1 day'::TEXT;

-- ============================================================
-- 13. DATE ARITHMETIC
-- ============================================================

\echo ''
\echo '13. DATE ARITHMETIC'

SELECT DATE '2026-01-15' + 1;

SELECT DATE '2026-01-15' + 7;

SELECT DATE '2026-01-15' - 1;

SELECT DATE '2026-01-15' - 7;

SELECT DATE '2026-01-15' + INTERVAL '1 day';

SELECT DATE '2026-01-15' + INTERVAL '1 month';

SELECT DATE '2026-01-15' + INTERVAL '1 year';

SELECT DATE '2026-01-15' - INTERVAL '1 day';

SELECT DATE '2026-01-15' - INTERVAL '1 month';

SELECT DATE '2026-01-15' - DATE '2026-01-01';

-- ============================================================
-- 14. TIMESTAMP ARITHMETIC
-- ============================================================

\echo ''
\echo '14. TIMESTAMP ARITHMETIC'

SELECT TIMESTAMP '2026-01-15 12:00:00'
       + INTERVAL '1 second';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       + INTERVAL '1 minute';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       + INTERVAL '1 hour';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       + INTERVAL '1 day';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       + INTERVAL '1 month';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       + INTERVAL '1 year';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       - INTERVAL '1 day';

SELECT TIMESTAMP '2026-01-15 12:00:00'
       - TIMESTAMP '2026-01-01 12:00:00';

-- ============================================================
-- 15. TIMESTAMPTZ ARITHMETIC
-- ============================================================

\echo ''
\echo '15. TIMESTAMPTZ ARITHMETIC'

SELECT TIMESTAMPTZ '2026-01-15 12:00:00+00'
       + INTERVAL '1 hour';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00+00'
       + INTERVAL '1 day';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00+00'
       + INTERVAL '1 month';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00+00'
       - INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    -
    TIMESTAMPTZ '2026-01-01 12:00:00+00';

-- ============================================================
-- 16. TIME ARITHMETIC
-- ============================================================

\echo ''
\echo '16. TIME ARITHMETIC'

SELECT TIME '12:00:00' + INTERVAL '1 hour';

SELECT TIME '12:00:00' + INTERVAL '30 minutes';

SELECT TIME '12:00:00' - INTERVAL '1 hour';

SELECT TIME '12:00:00' - TIME '10:00:00';

SELECT TIME '23:30:00' + INTERVAL '2 hours';

-- ============================================================
-- 17. INTERVAL ARITHMETIC
-- ============================================================

\echo ''
\echo '17. INTERVAL ARITHMETIC'

SELECT INTERVAL '1 day' + INTERVAL '2 hours';

SELECT INTERVAL '2 days' - INTERVAL '3 hours';

SELECT INTERVAL '1 hour' * 2;

SELECT 2 * INTERVAL '1 hour';

SELECT INTERVAL '10 hours' / 2;

SELECT -INTERVAL '2 hours';

SELECT INTERVAL '1 month' + INTERVAL '10 days';

-- ============================================================
-- 18. COMPARISONS
-- ============================================================

\echo ''
\echo '18. DATE/TIME COMPARISONS'

SELECT DATE '2026-01-01' < DATE '2026-01-02';

SELECT DATE '2026-01-01' = DATE '2026-01-01';

SELECT DATE '2026-01-02' > DATE '2026-01-01';

SELECT TIMESTAMP '2026-01-01 10:00:00'
       <
       TIMESTAMP '2026-01-01 11:00:00';

SELECT TIMESTAMPTZ '2026-01-01 10:00:00+00'
       =
       TIMESTAMPTZ '2026-01-01 15:30:00+05:30';

SELECT TIME '10:00:00' < TIME '11:00:00';

SELECT INTERVAL '1 hour' < INTERVAL '2 hours';

-- ============================================================
-- 19. EXTRACT
-- ============================================================

\echo ''
\echo '19. EXTRACT'

SELECT EXTRACT(YEAR FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(MONTH FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(DAY FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(HOUR FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(MINUTE FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(SECOND FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(MILLISECOND FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(MICROSECONDS FROM TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT EXTRACT(QUARTER FROM TIMESTAMP '2026-07-15 13:45:30');

SELECT EXTRACT(WEEK FROM TIMESTAMP '2026-07-15 13:45:30');

SELECT EXTRACT(DOW FROM TIMESTAMP '2026-07-15 13:45:30');

SELECT EXTRACT(ISODOW FROM TIMESTAMP '2026-07-15 13:45:30');

SELECT EXTRACT(DOY FROM TIMESTAMP '2026-07-15 13:45:30');

SELECT EXTRACT(EPOCH FROM TIMESTAMP '2026-07-15 13:45:30');

SELECT EXTRACT(EPOCH FROM TIMESTAMPTZ '2026-07-15 13:45:30+00');

SELECT EXTRACT(EPOCH FROM INTERVAL '2 days 3 hours');

SELECT EXTRACT(YEAR FROM INTERVAL '2 years 3 months');

SELECT EXTRACT(MONTH FROM INTERVAL '2 years 3 months');

SELECT EXTRACT(DAY FROM INTERVAL '10 days 3 hours');

SELECT EXTRACT(HOUR FROM INTERVAL '3 hours 20 minutes');

-- ============================================================
-- 20. DATE_PART
-- ============================================================

\echo ''
\echo '20. DATE_PART'

SELECT DATE_PART('year', TIMESTAMP '2026-07-15 13:45:30');

SELECT DATE_PART('month', TIMESTAMP '2026-07-15 13:45:30');

SELECT DATE_PART('day', TIMESTAMP '2026-07-15 13:45:30');

SELECT DATE_PART('hour', TIMESTAMP '2026-07-15 13:45:30');

SELECT DATE_PART('minute', TIMESTAMP '2026-07-15 13:45:30');

SELECT DATE_PART('second', TIMESTAMP '2026-07-15 13:45:30');

SELECT DATE_PART('epoch', TIMESTAMPTZ '2026-07-15 13:45:30+00');

SELECT DATE_PART('month', INTERVAL '2 years 3 months');

-- ============================================================
-- 21. DATE_TRUNC
-- ============================================================

\echo ''
\echo '21. DATE_TRUNC'

SELECT DATE_TRUNC(
    'microseconds',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'milliseconds',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'second',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'minute',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'hour',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'day',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'week',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'month',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'quarter',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'year',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'decade',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'century',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'millennium',
    TIMESTAMP '2026-07-15 13:45:30.123456'
);

SELECT DATE_TRUNC(
    'hour',
    INTERVAL '2 days 03:45:30'
);

-- ============================================================
-- 22. DATE_BIN
-- ============================================================

\echo ''
\echo '22. DATE_BIN'

SELECT DATE_BIN(
    INTERVAL '15 minutes',
    TIMESTAMP '2026-07-15 13:47:32',
    TIMESTAMP '2000-01-01 00:00:00'
);

SELECT DATE_BIN(
    INTERVAL '1 hour',
    TIMESTAMP '2026-07-15 13:47:32',
    TIMESTAMP '2000-01-01 00:00:00'
);

SELECT DATE_BIN(
    INTERVAL '1 day',
    TIMESTAMP '2026-07-15 13:47:32',
    TIMESTAMP '2000-01-01 00:00:00'
);

SELECT DATE_BIN(
    INTERVAL '15 minutes',
    TIMESTAMPTZ '2026-07-15 13:47:32+00',
    TIMESTAMPTZ '2000-01-01 00:00:00+00'
);

-- ============================================================
-- 23. AGE
-- ============================================================

\echo ''
\echo '23. AGE'

SELECT AGE(
    TIMESTAMP '2026-07-15',
    TIMESTAMP '2020-01-01'
);

SELECT AGE(
    TIMESTAMPTZ '2026-07-15 12:00:00+00',
    TIMESTAMPTZ '2020-01-01 12:00:00+00'
);

SELECT AGE(
    TIMESTAMP '2026-07-15'
);

SELECT AGE(
    DATE '2026-07-15'
);

-- ============================================================
-- 24. MAKE_DATE
-- ============================================================

\echo ''
\echo '24. MAKE_DATE'

SELECT MAKE_DATE(2026, 1, 15);

SELECT MAKE_DATE(2000, 2, 29);

SELECT MAKE_DATE(2024, 2, 29);

-- ============================================================
-- 25. MAKE_TIME
-- ============================================================

\echo ''
\echo '25. MAKE_TIME'

SELECT MAKE_TIME(12, 34, 56);

SELECT MAKE_TIME(12, 34, 56.123456);

-- ============================================================
-- 26. MAKE_TIMESTAMP
-- ============================================================

\echo ''
\echo '26. MAKE_TIMESTAMP'

SELECT MAKE_TIMESTAMP(
    2026, 1, 15, 12, 34, 56
);

SELECT MAKE_TIMESTAMP(
    2026, 1, 15, 12, 34, 56.123456
);

-- ============================================================
-- 27. MAKE_TIMESTAMPTZ
-- ============================================================

\echo ''
\echo '27. MAKE_TIMESTAMPTZ'

SELECT MAKE_TIMESTAMPTZ(
    2026, 1, 15, 12, 34, 56
);

SELECT MAKE_TIMESTAMPTZ(
    2026, 1, 15, 12, 34, 56.123456
);

SELECT MAKE_TIMESTAMPTZ(
    2026, 1, 15, 12, 34, 56,
    'Asia/Kolkata'
);

SELECT MAKE_TIMESTAMPTZ(
    2026, 1, 15, 12, 34, 56,
    'America/New_York'
);

-- ============================================================
-- 28. MAKE_INTERVAL
-- ============================================================

\echo ''
\echo '28. MAKE_INTERVAL'

SELECT MAKE_INTERVAL();

SELECT MAKE_INTERVAL(
    years => 1
);

SELECT MAKE_INTERVAL(
    months => 2
);

SELECT MAKE_INTERVAL(
    weeks => 3
);

SELECT MAKE_INTERVAL(
    days => 4
);

SELECT MAKE_INTERVAL(
    hours => 5
);

SELECT MAKE_INTERVAL(
    mins => 6
);

SELECT MAKE_INTERVAL(
    secs => 7
);

SELECT MAKE_INTERVAL(
    years => 1,
    months => 2,
    weeks => 3,
    days => 4,
    hours => 5,
    mins => 6,
    secs => 7.123456
);

-- ============================================================
-- 29. JUSTIFY FUNCTIONS
-- ============================================================

\echo ''
\echo '29. JUSTIFY FUNCTIONS'

SELECT JUSTIFY_DAYS(
    INTERVAL '40 days'
);

SELECT JUSTIFY_HOURS(
    INTERVAL '50 hours'
);

SELECT JUSTIFY_INTERVAL(
    INTERVAL '40 days 50 hours'
);

-- ============================================================
-- 30. ISFINITE
-- ============================================================

\echo ''
\echo '30. ISFINITE'

SELECT ISFINITE(DATE '2026-01-01');

SELECT ISFINITE(
    TIMESTAMP '2026-01-01 00:00:00'
);

SELECT ISFINITE(
    TIMESTAMPTZ '2026-01-01 00:00:00+00'
);

SELECT ISFINITE(
    INTERVAL '1 day'
);

SELECT ISFINITE(
    DATE 'infinity'
);

SELECT ISFINITE(
    TIMESTAMP 'infinity'
);

SELECT ISFINITE(
    TIMESTAMPTZ 'infinity'
);

-- ============================================================
-- 31. INFINITY / -INFINITY
-- ============================================================

\echo ''
\echo '31. INFINITY'

SELECT DATE 'infinity';

SELECT DATE '-infinity';

SELECT TIMESTAMP 'infinity';

SELECT TIMESTAMP '-infinity';

SELECT TIMESTAMPTZ 'infinity';

SELECT TIMESTAMPTZ '-infinity';

-- ============================================================
-- 32. AT TIME ZONE
-- ============================================================

\echo ''
\echo '32. AT TIME ZONE'

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'UTC';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    AT TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIME WITH TIME ZONE '12:00:00+00'
    AT TIME ZONE 'Asia/Kolkata';

-- ============================================================
-- 33. AT LOCAL
-- ============================================================

\echo ''
\echo '33. AT LOCAL'

SET TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT LOCAL;

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    AT LOCAL;

SELECT
    TIME WITH TIME ZONE '12:00:00+00'
    AT LOCAL;

-- ============================================================
-- 34. TIMEZONE FUNCTION
-- ============================================================

\echo ''
\echo '34. TIMEZONE FUNCTION'

SELECT TIMEZONE(
    'UTC',
    TIMESTAMP '2026-01-15 12:00:00'
);

SELECT TIMEZONE(
    'Asia/Kolkata',
    TIMESTAMP '2026-01-15 12:00:00'
);

SELECT TIMEZONE(
    'America/New_York',
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
);

-- ============================================================
-- 35. DST / IANA TIMEZONE
-- ============================================================

\echo ''
\echo '35. DST / IANA TIMEZONE'

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'Europe/London';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'Europe/London';

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'Australia/Sydney';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'Asia/Kolkata';

-- ============================================================
-- 36. DST ARITHMETIC
-- ============================================================

\echo ''
\echo '36. DST ARITHMETIC'

SET TIME ZONE 'America/Denver';

SELECT
    TIMESTAMPTZ '2026-03-08 01:30:00-07'
    + INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-03-08 01:30:00-07'
    + INTERVAL '24 hours';

SELECT
    TIMESTAMPTZ '2026-11-01 01:30:00-06'
    + INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-11-01 01:30:00-06'
    + INTERVAL '24 hours';

SET TIME ZONE 'UTC';

-- ============================================================
-- 37. DATE_ADD
-- ============================================================

\echo ''
\echo '37. DATE_ADD'

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 day'
);

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 month'
);

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 day',
    'Asia/Kolkata'
);

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 day',
    'America/New_York'
);

-- ============================================================
-- 38. DATE_SUBTRACT
-- ============================================================

\echo ''
\echo '38. DATE_SUBTRACT'

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 day'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 month'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 day',
    'Asia/Kolkata'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-01-15 12:00:00+00',
    INTERVAL '1 day',
    'America/New_York'
);

-- ============================================================
-- 39. GENERATE_SERIES - DATE
-- ============================================================

\echo ''
\echo '39. GENERATE_SERIES DATE/TIMESTAMP'

SELECT *
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-01 00:00:00',
    TIMESTAMP '2026-01-05 00:00:00',
    INTERVAL '1 day'
);

SELECT *
FROM GENERATE_SERIES(
    TIMESTAMPTZ '2026-01-01 00:00:00+00',
    TIMESTAMPTZ '2026-01-05 00:00:00+00',
    INTERVAL '1 day'
);

SELECT *
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-01 00:00:00',
    TIMESTAMP '2026-01-01 03:00:00',
    INTERVAL '30 minutes'
);

-- ============================================================
-- 40. GENERATE_SERIES WITH TABLE
-- ============================================================

\echo ''
\echo '40. GENERATE_SERIES + AGGREGATION'

SELECT
    COUNT(*)
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-01-10',
    INTERVAL '1 day'
);

SELECT
    MIN(x),
    MAX(x),
    COUNT(*)
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-01-10',
    INTERVAL '1 day'
) AS g(x);

-- ============================================================
-- 41. OVERLAPS
-- ============================================================

\echo ''
\echo '41. OVERLAPS'

SELECT
    (DATE '2026-01-01', DATE '2026-01-10')
    OVERLAPS
    (DATE '2026-01-05', DATE '2026-01-15');

SELECT
    (DATE '2026-01-01', DATE '2026-01-05')
    OVERLAPS
    (DATE '2026-01-05', DATE '2026-01-10');

SELECT
    (TIMESTAMP '2026-01-01 10:00',
     TIMESTAMP '2026-01-01 12:00')
    OVERLAPS
    (TIMESTAMP '2026-01-01 11:00',
     TIMESTAMP '2026-01-01 13:00');

SELECT
    (TIMESTAMP '2026-01-01 10:00',
     INTERVAL '2 hours')
    OVERLAPS
    (TIMESTAMP '2026-01-01 11:00',
     INTERVAL '2 hours');

-- ============================================================
-- 42. TO_CHAR - DATE
-- ============================================================

\echo ''
\echo '42. TO_CHAR DATE'

SELECT TO_CHAR(
    DATE '2026-07-15',
    'YYYY-MM-DD'
);

SELECT TO_CHAR(
    DATE '2026-07-15',
    'DD/MM/YYYY'
);

SELECT TO_CHAR(
    DATE '2026-07-15',
    'Day'
);

SELECT TO_CHAR(
    DATE '2026-07-15',
    'Month'
);

-- ============================================================
-- 43. TO_CHAR - TIMESTAMP
-- ============================================================

\echo ''
\echo '43. TO_CHAR TIMESTAMP'

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'YYYY-MM-DD HH24:MI:SS'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'YYYY-MM-DD HH24:MI:SS.US'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'YYYY-MM-DD HH12:MI:SS AM'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30',
    'Day, DD Month YYYY'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30',
    'YYYY "Week" WW'
);

-- ============================================================
-- 44. TO_CHAR - TIMESTAMPTZ
-- ============================================================

\echo ''
\echo '44. TO_CHAR TIMESTAMPTZ'

SELECT TO_CHAR(
    TIMESTAMPTZ '2026-07-15 13:45:30.123456+00',
    'YYYY-MM-DD HH24:MI:SS TZH:TZM'
);

SELECT TO_CHAR(
    TIMESTAMPTZ '2026-07-15 13:45:30.123456+00',
    'YYYY-MM-DD HH24:MI:SS.US OF'
);

-- ============================================================
-- 45. TO_CHAR - TIME
-- ============================================================

\echo ''
\echo '45. TO_CHAR TIME'

SELECT TO_CHAR(
    TIME '13:45:30.123456',
    'HH24:MI:SS'
);

SELECT TO_CHAR(
    TIME '13:45:30.123456',
    'HH24:MI:SS.US'
);

SELECT TO_CHAR(
    TIME '13:45:30',
    'HH12:MI:SS AM'
);

-- ============================================================
-- 46. TO_CHAR - INTERVAL
-- ============================================================

\echo ''
\echo '46. TO_CHAR INTERVAL'

SELECT TO_CHAR(
    INTERVAL '2 years 3 months 4 days 05:06:07',
    'YYYY "years" MM "months" DD "days" HH24:MI:SS'
);

-- ============================================================
-- 47. TO_DATE
-- ============================================================

\echo ''
\echo '47. TO_DATE'

SELECT TO_DATE(
    '2026-07-15',
    'YYYY-MM-DD'
);

SELECT TO_DATE(
    '15/07/2026',
    'DD/MM/YYYY'
);

SELECT TO_DATE(
    '20260715',
    'YYYYMMDD'
);

-- ============================================================
-- 48. TO_TIMESTAMP
-- ============================================================

\echo ''
\echo '48. TO_TIMESTAMP'

SELECT TO_TIMESTAMP(
    '2026-07-15 13:45:30',
    'YYYY-MM-DD HH24:MI:SS'
);

SELECT TO_TIMESTAMP(
    '15/07/2026 13:45:30',
    'DD/MM/YYYY HH24:MI:SS'
);

SELECT TO_TIMESTAMP(
    '20260715 134530',
    'YYYYMMDD HH24MISS'
);

-- ============================================================
-- 49. UNIX EPOCH CONVERSION
-- ============================================================

\echo ''
\echo '49. EPOCH CONVERSION'

SELECT EXTRACT(
    EPOCH FROM TIMESTAMPTZ '1970-01-01 00:00:00+00'
);

SELECT EXTRACT(
    EPOCH FROM TIMESTAMPTZ '2026-01-01 00:00:00+00'
);

SELECT TO_TIMESTAMP(0);

SELECT TO_TIMESTAMP(1);

SELECT TO_TIMESTAMP(1704067200);

-- ============================================================
-- 50. DATE/TIMESTAMP COALESCE
-- ============================================================

\echo ''
\echo '50. COALESCE'

SELECT COALESCE(
    NULL::DATE,
    DATE '2026-01-01'
);

SELECT COALESCE(
    NULL::TIMESTAMP,
    TIMESTAMP '2026-01-01 00:00:00'
);

SELECT COALESCE(
    NULL::TIMESTAMPTZ,
    TIMESTAMPTZ '2026-01-01 00:00:00+00'
);

SELECT COALESCE(
    NULL::TIME,
    TIME '12:00:00'
);

SELECT COALESCE(
    NULL::INTERVAL,
    INTERVAL '1 day'
);

-- ============================================================
-- 51. CASE
-- ============================================================

\echo ''
\echo '51. CASE'

SELECT CASE
    WHEN DATE '2026-01-01' < DATE '2027-01-01'
    THEN DATE '2026-01-01'
    ELSE DATE '2027-01-01'
END;

SELECT CASE
    WHEN CURRENT_TIMESTAMP IS NOT NULL
    THEN CURRENT_TIMESTAMP
END;

SELECT CASE
    WHEN TIMESTAMP '2026-01-01 10:00'
         < TIMESTAMP '2026-01-01 11:00'
    THEN 'before'
    ELSE 'after'
END;

-- ============================================================
-- 52. NULL DATE/TIME
-- ============================================================

\echo ''
\echo '52. NULL SEMANTICS'

SELECT NULL::DATE;

SELECT NULL::TIME;

SELECT NULL::TIMESTAMP;

SELECT NULL::TIMESTAMPTZ;

SELECT NULL::INTERVAL;

SELECT
    DATE '2026-01-01' + NULL::INTERVAL;

SELECT
    TIMESTAMP '2026-01-01' + NULL::INTERVAL;

SELECT
    NULL::TIMESTAMP AT TIME ZONE 'UTC';

-- ============================================================
-- 53. ORDERING
-- ============================================================

\echo ''
\echo '53. ORDERING'

CREATE TEMP TABLE datetime_order_test (
    id INTEGER,
    ts TIMESTAMP
);

INSERT INTO datetime_order_test VALUES
    (1, TIMESTAMP '2026-01-03'),
    (2, TIMESTAMP '2026-01-01'),
    (3, TIMESTAMP '2026-01-02'),
    (4, NULL);

SELECT *
FROM datetime_order_test
ORDER BY ts ASC NULLS FIRST;

SELECT *
FROM datetime_order_test
ORDER BY ts ASC NULLS LAST;

SELECT *
FROM datetime_order_test
ORDER BY ts DESC NULLS FIRST;

SELECT *
FROM datetime_order_test
ORDER BY ts DESC NULLS LAST;

-- ============================================================
-- 54. GROUPING
-- ============================================================

\echo ''
\echo '54. DATE GROUPING'

SELECT
    DATE_TRUNC('month', ts) AS month_start,
    COUNT(*)
FROM datetime_order_test
GROUP BY DATE_TRUNC('month', ts)
ORDER BY month_start;

-- ============================================================
-- 55. INDEX / WHERE DATE FILTER
-- ============================================================

\echo ''
\echo '55. DATE FILTERING'

CREATE INDEX datetime_order_test_ts_idx
ON datetime_order_test(ts);

SELECT *
FROM datetime_order_test
WHERE ts >= TIMESTAMP '2026-01-01'
  AND ts < TIMESTAMP '2026-01-04'
ORDER BY ts;

-- ============================================================
-- 56. TABLE DML WITH CURRENT_TIMESTAMP
-- ============================================================

\echo ''
\echo '56. DML CURRENT_TIMESTAMP'

CREATE TABLE plomid_datetime_test.events (
    id BIGINT PRIMARY KEY,
    event_name TEXT NOT NULL,
    created_date DATE,
    created_time TIME,
    created_ts TIMESTAMP,
    created_tstz TIMESTAMPTZ,
    duration INTERVAL
);

INSERT INTO plomid_datetime_test.events
VALUES (
    1,
    'event-one',
    CURRENT_DATE,
    CURRENT_TIME,
    CURRENT_TIMESTAMP::TIMESTAMP,
    CURRENT_TIMESTAMP,
    INTERVAL '1 hour'
);

INSERT INTO plomid_datetime_test.events
VALUES (
    2,
    'event-two',
    DATE '2026-01-01',
    TIME '10:30:00',
    TIMESTAMP '2026-01-01 10:30:00',
    TIMESTAMPTZ '2026-01-01 10:30:00+00',
    INTERVAL '2 days'
);

SELECT *
FROM plomid_datetime_test.events
ORDER BY id;

UPDATE plomid_datetime_test.events
SET
    created_ts = CURRENT_TIMESTAMP,
    created_tstz = CURRENT_TIMESTAMP,
    duration = duration + INTERVAL '1 hour'
WHERE id = 1;

SELECT *
FROM plomid_datetime_test.events
ORDER BY id;

-- ============================================================
-- 57. DATE/TIME IN EXPRESSIONS
-- ============================================================

\echo ''
\echo '57. EXPRESSIONS'

SELECT
    CURRENT_DATE + 7;

SELECT
    CURRENT_DATE - 7;

SELECT
    CURRENT_TIMESTAMP + INTERVAL '1 hour';

SELECT
    CURRENT_TIMESTAMP - INTERVAL '1 day';

SELECT
    DATE '2026-01-01'
    + TIME '12:30:00';

SELECT
    DATE '2026-01-01'
    + INTERVAL '12 hours';

SELECT
    TIMESTAMP '2026-01-01 12:00:00'
    + INTERVAL '90 minutes';

-- ============================================================
-- 58. MONTH-END ARITHMETIC
-- ============================================================

\echo ''
\echo '58. MONTH-END ARITHMETIC'

SELECT
    DATE '2026-01-31' + INTERVAL '1 month';

SELECT
    DATE '2026-02-28' + INTERVAL '1 month';

SELECT
    DATE '2024-01-31' + INTERVAL '1 month';

SELECT
    DATE '2024-02-29' + INTERVAL '1 month';

SELECT
    DATE '2026-03-31' - INTERVAL '1 month';

-- ============================================================
-- 59. LEAP YEAR
-- ============================================================

\echo ''
\echo '59. LEAP YEAR'

SELECT MAKE_DATE(2024, 2, 29);

SELECT DATE '2024-02-29' + 1;

SELECT DATE '2024-02-29' - 1;

SELECT EXTRACT(
    DOY FROM DATE '2024-12-31'
);

-- ============================================================
-- 60. WEEK / ISO WEEK
-- ============================================================

\echo ''
\echo '60. WEEK / ISO WEEK'

SELECT EXTRACT(
    WEEK FROM DATE '2026-01-01'
);

SELECT EXTRACT(
    ISODOW FROM DATE '2026-01-01'
);

SELECT EXTRACT(
    ISOYEAR FROM DATE '2026-01-01'
);

SELECT DATE_TRUNC(
    'week',
    TIMESTAMP '2026-01-01 12:34:56'
);

-- ============================================================
-- 61. TIMEZONE SETTING
-- ============================================================

\echo ''
\echo '61. TIMEZONE SETTING'

SET TIME ZONE 'UTC';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE 'Asia/Kolkata';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE 'America/New_York';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE 'Europe/London';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE 'UTC';

-- ============================================================
-- 62. TIMESTAMP EQUALITY ACROSS TIMEZONES
-- ============================================================

\echo ''
\echo '62. TIMESTAMPTZ EQUALITY'

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    =
    TIMESTAMPTZ '2026-01-15 17:30:00+05:30';

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    <
    TIMESTAMPTZ '2026-01-15 13:00:00+00';

-- ============================================================
-- 63. TIMESTAMP WITHOUT TIMEZONE VS TIMESTAMPTZ
-- ============================================================

\echo ''
\echo '63. TIMESTAMP VS TIMESTAMPTZ'

SET TIME ZONE 'UTC';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    =
    TIMESTAMPTZ '2026-01-15 12:00:00+00';

SET TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    =
    TIMESTAMPTZ '2026-01-15 12:00:00+00';

SET TIME ZONE 'UTC';

-- ============================================================
-- 64. DATE/TIME TYPE INFORMATION
-- ============================================================

\echo ''
\echo '64. TYPE INFORMATION'

SELECT pg_typeof(DATE '2026-01-01');

SELECT pg_typeof(TIME '12:00:00');

SELECT pg_typeof(TIMESTAMP '2026-01-01 12:00:00');

SELECT pg_typeof(
    TIMESTAMPTZ '2026-01-01 12:00:00+00'
);

SELECT pg_typeof(INTERVAL '1 day');

SELECT pg_typeof(CURRENT_DATE);

SELECT pg_typeof(CURRENT_TIME);

SELECT pg_typeof(CURRENT_TIMESTAMP);

SELECT pg_typeof(NOW());

SELECT pg_typeof(
    DATE '2026-01-01' + INTERVAL '1 day'
);

SELECT pg_typeof(
    TIMESTAMP '2026-01-01' - TIMESTAMP '2025-01-01'
);

-- ============================================================
-- 65. INFORMATION_SCHEMA COLUMN TYPES
-- ============================================================

\echo ''
\echo '65. INFORMATION_SCHEMA'

SELECT
    column_name,
    data_type,
    datetime_precision,
    interval_type,
    interval_precision
FROM information_schema.columns
WHERE table_schema = 'plomid_datetime_test'
  AND table_name = 'all_datetime_types'
ORDER BY ordinal_position;

-- ============================================================
-- 66. FINAL DATA VALIDATION
-- ============================================================

\echo ''
\echo '66. FINAL DATA VALIDATION'

SELECT
    COUNT(*)
FROM plomid_datetime_test.all_datetime_types;

SELECT
    COUNT(*)
FROM plomid_datetime_test.events;





-- ============================================================
-- 69. DATESTYLE
-- ============================================================

\echo ''
\echo '69. DATESTYLE'

SHOW DateStyle;

SET DateStyle = 'ISO, MDY';

SELECT DATE '2026-01-15';

SELECT '01/02/2026'::DATE;

SET DateStyle = 'ISO, DMY';

SELECT '01/02/2026'::DATE;

SET DateStyle = 'ISO, YMD';

SELECT '2026-01-02'::DATE;

SET DateStyle = 'ISO, MDY';

-- ============================================================
-- 70. INTERVALSTYLE
-- ============================================================

\echo ''
\echo '70. INTERVALSTYLE'

SHOW IntervalStyle;

SET IntervalStyle = 'postgres';

SELECT INTERVAL '1 year 2 mons 3 days 04:05:06';

SET IntervalStyle = 'postgres_verbose';

SELECT INTERVAL '1 year 2 mons 3 days 04:05:06';

SET IntervalStyle = 'sql_standard';

SELECT INTERVAL '1 year 2 mons 3 days 04:05:06';

SET IntervalStyle = 'iso_8601';

SELECT INTERVAL '1 year 2 mons 3 days 04:05:06';

SET IntervalStyle = 'postgres';

-- ============================================================
-- 71. COMPLETE INTERVAL QUALIFIERS
-- ============================================================

\echo ''
\echo '71. COMPLETE INTERVAL QUALIFIERS'

SELECT INTERVAL '1' YEAR;

SELECT INTERVAL '2' MONTH;

SELECT INTERVAL '3' DAY;

SELECT INTERVAL '4' HOUR;

SELECT INTERVAL '5' MINUTE;

SELECT INTERVAL '6' SECOND;

SELECT INTERVAL '1-2' YEAR TO MONTH;

SELECT INTERVAL '3 04' DAY TO HOUR;

SELECT INTERVAL '3 04:05' DAY TO MINUTE;

SELECT INTERVAL '3 04:05:06' DAY TO SECOND;

SELECT INTERVAL '04:05' HOUR TO MINUTE;

SELECT INTERVAL '04:05:06' HOUR TO SECOND;

SELECT INTERVAL '05:06' MINUTE TO SECOND;

-- ============================================================
-- 72. ISO-8601 INTERVAL INPUT
-- ============================================================

\echo ''
\echo '72. ISO-8601 INTERVAL INPUT'

SELECT INTERVAL 'P1Y';

SELECT INTERVAL 'P2M';

SELECT INTERVAL 'P3D';

SELECT INTERVAL 'PT4H';

SELECT INTERVAL 'PT5M';

SELECT INTERVAL 'PT6S';

SELECT INTERVAL 'P1Y2M3DT4H5M6S';

SELECT INTERVAL 'P0001-02-03T04:05:06';

-- ============================================================
-- 73. POSTGRES INTERVAL INPUT FORMS
-- ============================================================

\echo ''
\echo '73. POSTGRES INTERVAL INPUT FORMS'

SELECT INTERVAL '@ 1 year 2 mons';

SELECT INTERVAL '@ 3 days 04:05:06';

SELECT INTERVAL '1 year 2 mons ago';

SELECT INTERVAL '3 days ago';

SELECT INTERVAL '04:05:06';

SELECT INTERVAL '1 day 12:59:10';

SELECT INTERVAL '200-10';

-- ============================================================
-- 74. SPECIAL DATE/TIME INPUT VALUES
-- ============================================================

\echo ''
\echo '74. SPECIAL DATE/TIME INPUT VALUES'

SELECT 'epoch'::TIMESTAMP;

SELECT 'epoch'::TIMESTAMPTZ;

SELECT 'now'::TIMESTAMP;

SELECT 'now'::TIMESTAMPTZ;

SELECT 'today'::DATE;

SELECT 'today'::TIMESTAMP;

SELECT 'tomorrow'::DATE;

SELECT 'tomorrow'::TIMESTAMP;

SELECT 'yesterday'::DATE;

SELECT 'yesterday'::TIMESTAMP;

SELECT 'allballs'::TIME;

-- ============================================================
-- 75. TIME INPUT FORMATS
-- ============================================================

\echo ''
\echo '75. TIME INPUT FORMATS'

SELECT TIME '040506';

SELECT TIME '04:05:06';

SELECT TIME '04:05 PM';

SELECT TIME '04:05:06 PM';

SELECT TIME '12:00 AM';

SELECT TIME '12:00 PM';

SELECT TIME '24:00:00';

SELECT TIME '04:05:06.123';

SELECT TIME '04:05:06.123456';

-- ============================================================
-- 76. TIMESTAMP INPUT FORMATS
-- ============================================================

\echo ''
\echo '76. TIMESTAMP INPUT FORMATS'

SELECT TIMESTAMP '2026-01-15';

SELECT TIMESTAMP '2026-01-15 04:05:06';

SELECT TIMESTAMP '2026-01-15 04:05:06.123456';

SELECT TIMESTAMP '2026-01-15 04:05 PM';

SELECT TIMESTAMP '2026-01-15 04:05:06 PM';

SELECT TIMESTAMP '2026-01-15 04:05:06 UTC';

SELECT TIMESTAMP '2026-01-15 04:05:06+05:30';

-- ============================================================
-- 77. TIMESTAMPTZ INPUT FORMATS
-- ============================================================

\echo ''
\echo '77. TIMESTAMPTZ INPUT FORMATS'

SELECT TIMESTAMPTZ '2026-01-15 04:05:06 UTC';

SELECT TIMESTAMPTZ '2026-01-15 04:05:06+00';

SELECT TIMESTAMPTZ '2026-01-15 04:05:06+05:30';

SELECT TIMESTAMPTZ '2026-01-15 04:05:06-05';

SELECT TIMESTAMPTZ '2026-01-15 04:05:06 America/New_York';

SELECT TIMESTAMPTZ '2026-01-15 04:05:06 Europe/London';

SELECT TIMESTAMPTZ '2026-01-15 04:05:06 Asia/Kolkata';

-- ============================================================
-- 78. TIMEZONE ABBREVIATIONS
-- ============================================================

\echo ''
\echo '78. TIMEZONE ABBREVIATIONS'

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 UTC';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 GMT';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 EST';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 EDT';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 PST';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 PDT';

SELECT TIMESTAMPTZ '2026-01-15 12:00:00 Z';

-- ============================================================
-- 79. TIMEZONE CATALOGS
-- ============================================================

\echo ''
\echo '79. TIMEZONE CATALOGS'

SELECT COUNT(*)
FROM pg_timezone_names;

SELECT COUNT(*)
FROM pg_timezone_abbrevs;

SELECT name, abbrev, utc_offset, is_dst
FROM pg_timezone_names
WHERE name IN (
    'UTC',
    'Asia/Kolkata',
    'America/New_York',
    'Europe/London'
)
ORDER BY name;

SELECT abbrev, utc_offset, is_dst
FROM pg_timezone_abbrevs
WHERE abbrev IN ('UTC', 'EST', 'EDT', 'PST', 'PDT')
ORDER BY abbrev;

-- ============================================================
-- 80. EXTRACT ADDITIONAL FIELDS
-- ============================================================

\echo ''
\echo '80. EXTRACT ADDITIONAL FIELDS'

SELECT EXTRACT(
    DECADE FROM TIMESTAMP '2026-07-15 13:45:30'
);

SELECT EXTRACT(
    CENTURY FROM TIMESTAMP '2026-07-15 13:45:30'
);

SELECT EXTRACT(
    MILLENNIUM FROM TIMESTAMP '2026-07-15 13:45:30'
);

SELECT EXTRACT(
    ISOYEAR FROM TIMESTAMP '2026-07-15 13:45:30'
);

SELECT EXTRACT(
    TIMEZONE FROM TIMESTAMPTZ '2026-07-15 13:45:30+05:30'
);

SELECT EXTRACT(
    TIMEZONE_HOUR FROM TIMESTAMPTZ '2026-07-15 13:45:30+05:30'
);

SELECT EXTRACT(
    TIMEZONE_MINUTE FROM TIMESTAMPTZ '2026-07-15 13:45:30+05:30'
);

-- ============================================================
-- 81. DATE_PART ADDITIONAL FIELDS
-- ============================================================

\echo ''
\echo '81. DATE_PART ADDITIONAL FIELDS'

SELECT DATE_PART(
    'decade',
    TIMESTAMP '2026-07-15 13:45:30'
);

SELECT DATE_PART(
    'century',
    TIMESTAMP '2026-07-15 13:45:30'
);

SELECT DATE_PART(
    'millennium',
    TIMESTAMP '2026-07-15 13:45:30'
);

SELECT DATE_PART(
    'isoyear',
    TIMESTAMP '2026-07-15 13:45:30'
);

SELECT DATE_PART(
    'timezone',
    TIMESTAMPTZ '2026-07-15 13:45:30+05:30'
);

SELECT DATE_PART(
    'timezone_hour',
    TIMESTAMPTZ '2026-07-15 13:45:30+05:30'
);

SELECT DATE_PART(
    'timezone_minute',
    TIMESTAMPTZ '2026-07-15 13:45:30+05:30'
);

-- ============================================================
-- 82. DATE_TRUNC WITH EXPLICIT TIMEZONE
-- ============================================================

\echo ''
\echo '82. DATE_TRUNC WITH TIMEZONE'

SELECT DATE_TRUNC(
    'day',
    TIMESTAMPTZ '2026-07-15 13:45:30+00',
    'Asia/Kolkata'
);

SELECT DATE_TRUNC(
    'day',
    TIMESTAMPTZ '2026-07-15 13:45:30+00',
    'America/New_York'
);

SELECT DATE_TRUNC(
    'month',
    TIMESTAMPTZ '2026-07-15 13:45:30+00',
    'Europe/London'
);

SELECT DATE_TRUNC(
    'hour',
    TIMESTAMPTZ '2026-07-15 13:45:30+00',
    'Australia/Sydney'
);

-- ============================================================
-- 83. NESTED AT TIME ZONE
-- ============================================================

\echo ''
\echo '83. NESTED AT TIME ZONE'

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'Asia/Tokyo'
    AT TIME ZONE 'America/Chicago';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    AT TIME ZONE 'UTC'
    AT TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    AT TIME ZONE 'America/New_York'
    AT TIME ZONE 'UTC';

-- ============================================================
-- 84. DST SPRING-FORWARD EDGE CASE
-- ============================================================

\echo ''
\echo '84. DST SPRING-FORWARD'

SET TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-03-08 01:30:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-03-08 02:30:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-03-08 03:30:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMPTZ '2026-03-08 06:30:00+00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMPTZ '2026-03-08 07:30:00+00'
    AT TIME ZONE 'America/New_York';

-- ============================================================
-- 85. DST FALL-BACK EDGE CASE
-- ============================================================

\echo ''
\echo '85. DST FALL-BACK'

SELECT
    TIMESTAMP '2026-11-01 00:30:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-11-01 01:30:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMP '2026-11-01 02:30:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMPTZ '2026-11-01 05:30:00+00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMPTZ '2026-11-01 06:30:00+00'
    AT TIME ZONE 'America/New_York';

-- ============================================================
-- 86. DST 1 DAY VS 24 HOURS
-- ============================================================

\echo ''
\echo '86. DST 1 DAY VS 24 HOURS'

SELECT
    TIMESTAMPTZ '2026-03-07 12:00:00-05'
    + INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-03-07 12:00:00-05'
    + INTERVAL '24 hours';

SELECT
    TIMESTAMPTZ '2026-11-01 12:00:00-05'
    + INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-11-01 12:00:00-05'
    + INTERVAL '24 hours';

-- ============================================================
-- 87. BC DATES
-- ============================================================

\echo ''
\echo '87. BC DATES'

SET TIME ZONE 'UTC';

SELECT DATE '0001-01-01 BC';

SELECT DATE '0044-03-15 BC';

SELECT DATE '4713-01-01 BC';

SELECT TIMESTAMP '0001-01-01 00:00:00 BC';

SELECT TIMESTAMP '0044-03-15 12:30:00 BC';

SELECT TIMESTAMPTZ '0001-01-01 00:00:00 BC';

SELECT
    DATE '0001-01-01 BC'
    + INTERVAL '1 day';

-- ============================================================
-- 88. DATE/TIMESTAMP BOUNDARIES
-- ============================================================

\echo ''
\echo '88. DATE/TIMESTAMP BOUNDARIES'

SELECT DATE '4713-01-01 BC';

SELECT DATE '5874897-12-31';

SELECT TIMESTAMP '4713-01-01 00:00:00 BC';

SELECT TIMESTAMP '294276-12-31 23:59:59.999999';

SELECT TIMESTAMPTZ '294276-12-31 23:59:59.999999+00';

-- ============================================================
-- 89. NEGATIVE INTERVALS
-- ============================================================

\echo ''
\echo '89. NEGATIVE INTERVALS'

SELECT INTERVAL '-1 day';

SELECT INTERVAL '-1 hour';

SELECT INTERVAL '-1 year';

SELECT INTERVAL '-1 year -2 months';

SELECT INTERVAL '-1 day -02:03:04';

SELECT INTERVAL '1 day -02:03:04';

SELECT -INTERVAL '2 days';

SELECT INTERVAL '-2 hours' * 3;

-- ============================================================
-- 90. FRACTIONAL INTERVALS
-- ============================================================

\echo ''
\echo '90. FRACTIONAL INTERVALS'

SELECT INTERVAL '1.5 years';

SELECT INTERVAL '1.5 months';

SELECT INTERVAL '1.5 weeks';

SELECT INTERVAL '1.75 days';

SELECT INTERVAL '1.5 hours';

SELECT INTERVAL '1.5 minutes';

SELECT INTERVAL '1.5 seconds';

-- ============================================================
-- 91. INTERVAL NORMALIZATION
-- ============================================================

\echo ''
\echo '91. INTERVAL NORMALIZATION'

SELECT INTERVAL '15 months';

SELECT INTERVAL '48 hours';

SELECT INTERVAL '120 minutes';

SELECT INTERVAL '90 seconds';

SELECT JUSTIFY_DAYS(INTERVAL '60 days');

SELECT JUSTIFY_HOURS(INTERVAL '72 hours');

SELECT JUSTIFY_INTERVAL(
    INTERVAL '60 days 72 hours'
);

-- ============================================================
-- 92. MAKE_INTERVAL POSITIONAL ARGUMENTS
-- ============================================================

\echo ''
\echo '92. MAKE_INTERVAL POSITIONAL'

SELECT MAKE_INTERVAL(
    1,
    2,
    3,
    4,
    5,
    6,
    7
);

SELECT MAKE_INTERVAL(
    2,
    3,
    0,
    10,
    5,
    30,
    45.123456
);

-- ============================================================
-- 93. GENERATE_SERIES EDGE CASES
-- ============================================================

\echo ''
\echo '93. GENERATE_SERIES EDGE CASES'

SELECT *
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-05',
    TIMESTAMP '2026-01-01',
    INTERVAL '-1 day'
);

SELECT *
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-01-05',
    INTERVAL '2 days'
);

SELECT *
FROM GENERATE_SERIES(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-01-01',
    INTERVAL '1 day'
);

SELECT COUNT(*)
FROM GENERATE_SERIES(
    TIMESTAMPTZ '2026-01-01 00:00:00+00',
    TIMESTAMPTZ '2026-01-05 00:00:00+00',
    INTERVAL '12 hours'
);

-- ============================================================
-- 94. DATE/TIME BETWEEN
-- ============================================================

\echo ''
\echo '94. BETWEEN'

SELECT
    DATE '2026-01-15'
    BETWEEN DATE '2026-01-01'
    AND DATE '2026-01-31';

SELECT
    TIMESTAMP '2026-01-15 12:00:00'
    BETWEEN TIMESTAMP '2026-01-01'
    AND TIMESTAMP '2026-01-31';

SELECT
    TIMESTAMPTZ '2026-01-15 12:00:00+00'
    BETWEEN
    TIMESTAMPTZ '2026-01-01 00:00:00+00'
    AND
    TIMESTAMPTZ '2026-01-31 23:59:59+00';

-- ============================================================
-- 95. DATE/TIME IN
-- ============================================================

\echo ''
\echo '95. IN'

SELECT DATE '2026-01-15'
IN (
    DATE '2026-01-01',
    DATE '2026-01-15',
    DATE '2026-01-31'
);

SELECT TIMESTAMP '2026-01-15 12:00:00'
IN (
    TIMESTAMP '2026-01-01 12:00:00',
    TIMESTAMP '2026-01-15 12:00:00',
    TIMESTAMP '2026-01-31 12:00:00'
);

-- ============================================================
-- 96. IS DISTINCT FROM
-- ============================================================

\echo ''
\echo '96. IS DISTINCT FROM'

SELECT
    DATE '2026-01-01'
    IS DISTINCT FROM DATE '2026-01-02';

SELECT
    DATE '2026-01-01'
    IS NOT DISTINCT FROM DATE '2026-01-01';

SELECT
    NULL::DATE
    IS DISTINCT FROM DATE '2026-01-01';

SELECT
    NULL::TIMESTAMP
    IS NOT DISTINCT FROM NULL::TIMESTAMP;

SELECT
    NULL::TIMESTAMPTZ
    IS NOT DISTINCT FROM NULL::TIMESTAMPTZ;

-- ============================================================
-- 97. DATE/TIME UNION TYPE RESOLUTION
-- ============================================================

\echo ''
\echo '97. UNION TYPE RESOLUTION'

SELECT DATE '2026-01-01'
UNION ALL
SELECT DATE '2026-01-02';

SELECT TIMESTAMP '2026-01-01 10:00:00'
UNION ALL
SELECT TIMESTAMP '2026-01-02 10:00:00';

SELECT TIMESTAMPTZ '2026-01-01 10:00:00+00'
UNION ALL
SELECT TIMESTAMPTZ '2026-01-02 10:00:00+00';

-- ============================================================
-- 98. WINDOW FUNCTIONS
-- ============================================================

\echo ''
\echo '98. DATE/TIME WINDOW FUNCTIONS'

CREATE TEMP TABLE datetime_window_test (
    id INTEGER,
    ts TIMESTAMP
);

INSERT INTO datetime_window_test VALUES
    (1, TIMESTAMP '2026-01-01 10:00:00'),
    (2, TIMESTAMP '2026-01-01 11:00:00'),
    (3, TIMESTAMP '2026-01-01 12:00:00'),
    (4, TIMESTAMP '2026-01-01 13:00:00');

SELECT
    id,
    ts,
    LAG(ts) OVER (ORDER BY ts) AS previous_ts,
    LEAD(ts) OVER (ORDER BY ts) AS next_ts
FROM datetime_window_test
ORDER BY id;

SELECT
    id,
    ts,
    MIN(ts) OVER (ORDER BY ts) AS min_ts,
    MAX(ts) OVER (ORDER BY ts) AS max_ts
FROM datetime_window_test
ORDER BY id;

-- ============================================================
-- 99. DATE/TIME AGGREGATES
-- ============================================================

\echo ''
\echo '99. DATE/TIME AGGREGATES'

SELECT MIN(ts), MAX(ts)
FROM datetime_window_test;

SELECT COUNT(ts)
FROM datetime_window_test;

SELECT MIN(DATE '2026-01-01'),
       MAX(DATE '2026-01-31');

SELECT MIN(TIME '10:00:00'),
       MAX(TIME '20:00:00');

SELECT
    MIN(TIMESTAMPTZ '2026-01-01 00:00:00+00'),
    MAX(TIMESTAMPTZ '2026-01-31 00:00:00+00');

-- ============================================================
-- 100. DISTINCT DATE/TIME
-- ============================================================

\echo ''
\echo '100. DISTINCT'

SELECT DISTINCT ts
FROM datetime_window_test
ORDER BY ts;

-- ============================================================
-- 101. ADDITIONAL TO_CHAR FORMAT TOKENS
-- ============================================================

\echo ''
\echo '101. TO_CHAR FORMAT TOKENS'

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'YYYY YYY YY Y'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'IYYY IYY IW ID IDDD'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'MM MON MONTH'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'DD DDD DY DAY'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'HH HH12 HH24 MI SS'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'MS US'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'AM PM'
);

SELECT TO_CHAR(
    TIMESTAMPTZ '2026-07-15 13:45:30.123456+05:30',
    'TZ TZH TZM OF'
);

-- ============================================================
-- 102. TO_DATE ADDITIONAL FORMATS
-- ============================================================

\echo ''
\echo '102. TO_DATE ADDITIONAL FORMATS'

SELECT TO_DATE(
    '2026 196 15',
    'YYYY DDD DD'
);

SELECT TO_DATE(
    '15-Jul-2026',
    'DD-Mon-YYYY'
);

SELECT TO_DATE(
    '15 JULY 2026',
    'DD MONTH YYYY'
);

-- ============================================================
-- 103. TO_TIMESTAMP TIMEZONE FORMATS
-- ============================================================

\echo ''
\echo '103. TO_TIMESTAMP TIMEZONE FORMATS'

SELECT TO_TIMESTAMP(
    '2026-07-15 13:45:30+05:30',
    'YYYY-MM-DD HH24:MI:SSTZH:TZM'
);

SELECT TO_TIMESTAMP(
    '2026-07-15 13:45:30 UTC',
    'YYYY-MM-DD HH24:MI:SS TZ'
);

-- ============================================================
-- 104. DATE/TIME TYPE RESOLUTION
-- ============================================================

\echo ''
\echo '104. TYPE RESOLUTION'

SELECT pg_typeof(
    DATE '2026-01-01' + 1
);

SELECT pg_typeof(
    DATE '2026-01-01' + INTERVAL '1 day'
);

SELECT pg_typeof(
    TIMESTAMP '2026-01-01' + INTERVAL '1 day'
);

SELECT pg_typeof(
    TIMESTAMPTZ '2026-01-01 00:00:00+00'
    + INTERVAL '1 day'
);

SELECT pg_typeof(
    TIMESTAMP '2026-01-02'
    - TIMESTAMP '2026-01-01'
);

SELECT pg_typeof(
    DATE '2026-01-02'
    - DATE '2026-01-01'
);

-- ============================================================
-- 105. FINAL EXTENDED CLEANUP
-- ============================================================

\echo ''
\echo '105. EXTENDED CLEANUP'

DROP SCHEMA IF EXISTS plomid_datetime_test CASCADE;

SELECT COUNT(*)
FROM information_schema.tables
WHERE table_schema = 'plomid_datetime_test';

-- APPEND-ONLY EXTENSION
-- Sections 106+
--
-- Purpose:
--   Cover remaining PostgreSQL 17 date/time compatibility areas:
--   transaction/statement timestamps, TIME WITH TIME ZONE,
--   complete cast matrix, arithmetic operators, DST behavior,
--   timezone/session behavior, NULL propagation, type resolution,
--   prepared parameters, temporal predicates, aggregates,
--   catalog introspection and additional edge cases.
--
-- Target:
--   PostgreSQL 17
--
-- IMPORTANT:
--   These tests intentionally exercise PostgreSQL semantics.
--   Do not weaken/remove tests merely because PLOMID currently
--   does not support a feature.
-- ============================================================

\set ON_ERROR_STOP on

-- ============================================================
-- 106. TRANSACTION / STATEMENT / CLOCK TIMESTAMP FUNCTIONS
-- ============================================================

SELECT NOW();
SELECT CURRENT_TIMESTAMP;
SELECT TRANSACTION_TIMESTAMP();
SELECT STATEMENT_TIMESTAMP();
SELECT CLOCK_TIMESTAMP();

SELECT
    NOW() AS now_value,
    CURRENT_TIMESTAMP AS current_timestamp_value,
    TRANSACTION_TIMESTAMP() AS transaction_timestamp_value,
    STATEMENT_TIMESTAMP() AS statement_timestamp_value,
    CLOCK_TIMESTAMP() AS clock_timestamp_value;

BEGIN;

SELECT
    NOW() AS now_before_sleep,
    TRANSACTION_TIMESTAMP() AS transaction_before_sleep,
    STATEMENT_TIMESTAMP() AS statement_before_sleep,
    CLOCK_TIMESTAMP() AS clock_before_sleep;

SELECT pg_sleep(0.01);

SELECT
    NOW() AS now_after_sleep,
    TRANSACTION_TIMESTAMP() AS transaction_after_sleep,
    STATEMENT_TIMESTAMP() AS statement_after_sleep,
    CLOCK_TIMESTAMP() AS clock_after_sleep;

COMMIT;

BEGIN;

SELECT NOW() AS transaction_1_start;

SELECT pg_sleep(0.01);

SELECT NOW() AS transaction_1_after_sleep;

COMMIT;

BEGIN;

SELECT NOW() AS transaction_2_start;

COMMIT;


-- ============================================================
-- 107. CURRENT DATE/TIME PRECISION
-- ============================================================

SELECT CURRENT_TIMESTAMP;
SELECT CURRENT_TIMESTAMP(0);
SELECT CURRENT_TIMESTAMP(1);
SELECT CURRENT_TIMESTAMP(2);
SELECT CURRENT_TIMESTAMP(3);
SELECT CURRENT_TIMESTAMP(4);
SELECT CURRENT_TIMESTAMP(5);
SELECT CURRENT_TIMESTAMP(6);

SELECT CURRENT_TIME;
SELECT CURRENT_TIME(0);
SELECT CURRENT_TIME(1);
SELECT CURRENT_TIME(2);
SELECT CURRENT_TIME(3);
SELECT CURRENT_TIME(4);
SELECT CURRENT_TIME(5);
SELECT CURRENT_TIME(6);

SELECT LOCALTIME;
SELECT LOCALTIME(0);
SELECT LOCALTIME(1);
SELECT LOCALTIME(2);
SELECT LOCALTIME(3);
SELECT LOCALTIME(4);
SELECT LOCALTIME(5);
SELECT LOCALTIME(6);

SELECT LOCALTIMESTAMP;
SELECT LOCALTIMESTAMP(0);
SELECT LOCALTIMESTAMP(1);
SELECT LOCALTIMESTAMP(2);
SELECT LOCALTIMESTAMP(3);
SELECT LOCALTIMESTAMP(4);
SELECT LOCALTIMESTAMP(5);
SELECT LOCALTIMESTAMP(6);


-- ============================================================
-- 108. TIME WITHOUT TIME ZONE
-- ============================================================

SELECT TIME '00:00:00';
SELECT TIME '12:00:00';
SELECT TIME '23:59:59';
SELECT TIME '24:00:00';

SELECT TIME '12:34:56.123456';

SELECT TIME(0) '12:34:56.123456';
SELECT TIME(1) '12:34:56.123456';
SELECT TIME(2) '12:34:56.123456';
SELECT TIME(3) '12:34:56.123456';
SELECT TIME(4) '12:34:56.123456';
SELECT TIME(5) '12:34:56.123456';
SELECT TIME(6) '12:34:56.123456';

SELECT
    TIME '12:00:00' + INTERVAL '1 hour';

SELECT
    TIME '12:00:00' - INTERVAL '1 hour';

SELECT
    TIME '00:30:00' - INTERVAL '1 hour';

SELECT
    TIME '23:30:00' + INTERVAL '2 hours';

SELECT
    TIME '12:00:00' - TIME '10:30:00';

SELECT
    TIME '23:59:59' - TIME '00:00:01';


-- ============================================================
-- 109. TIME WITH TIME ZONE COMPLETE COVERAGE
-- ============================================================

SELECT TIMETZ '00:00:00+00';
SELECT TIMETZ '12:00:00+00';
SELECT TIMETZ '23:59:59+05:30';
SELECT TIMETZ '12:34:56.123456+05:30';
SELECT TIMETZ '12:34:56-04:00';
SELECT TIMETZ '12:34:56Z';

SELECT TIME WITH TIME ZONE '12:00:00+05:30';

SELECT TIMETZ(0) '12:34:56.123456+05:30';
SELECT TIMETZ(1) '12:34:56.123456+05:30';
SELECT TIMETZ(2) '12:34:56.123456+05:30';
SELECT TIMETZ(3) '12:34:56.123456+05:30';
SELECT TIMETZ(4) '12:34:56.123456+05:30';
SELECT TIMETZ(5) '12:34:56.123456+05:30';
SELECT TIMETZ(6) '12:34:56.123456+05:30';

SELECT TIMETZ '12:00:00+05:30' + INTERVAL '1 hour';
SELECT TIMETZ '12:00:00+05:30' - INTERVAL '1 hour';

SELECT TIMETZ '12:00:00+05:30' + INTERVAL '30 minutes';
SELECT TIMETZ '12:00:00+05:30' - INTERVAL '30 minutes';

SELECT TIMETZ '00:30:00+05:30' - INTERVAL '1 hour';
SELECT TIMETZ '23:30:00+05:30' + INTERVAL '2 hours';

SELECT TIMETZ '12:00:00+05:30' AT TIME ZONE 'UTC';
SELECT TIMETZ '12:00:00+05:30' AT TIME ZONE 'America/New_York';

SELECT TIMETZ '12:00:00+05:30' = TIMETZ '06:30:00+00';
SELECT TIMETZ '12:00:00+05:30' <> TIMETZ '06:00:00+00';
SELECT TIMETZ '12:00:00+05:30' > TIMETZ '05:00:00+00';
SELECT TIMETZ '12:00:00+05:30' < TIMETZ '07:00:00+00';

SELECT EXTRACT(HOUR FROM TIMETZ '12:34:56+05:30');
SELECT EXTRACT(MINUTE FROM TIMETZ '12:34:56+05:30');
SELECT EXTRACT(SECOND FROM TIMETZ '12:34:56.123456+05:30');
SELECT EXTRACT(MILLISECONDS FROM TIMETZ '12:34:56.123456+05:30');
SELECT EXTRACT(MICROSECONDS FROM TIMETZ '12:34:56.123456+05:30');
SELECT EXTRACT(TIMEZONE FROM TIMETZ '12:34:56+05:30');
SELECT EXTRACT(TIMEZONE_HOUR FROM TIMETZ '12:34:56+05:30');
SELECT EXTRACT(TIMEZONE_MINUTE FROM TIMETZ '12:34:56+05:30');

SELECT DATE_PART('hour', TIMETZ '12:34:56+05:30');
SELECT DATE_PART('minute', TIMETZ '12:34:56+05:30');
SELECT DATE_PART('second', TIMETZ '12:34:56.123456+05:30');
SELECT DATE_PART('timezone', TIMETZ '12:34:56+05:30');

SELECT TO_CHAR(TIMETZ '12:34:56.123456+05:30',
               'HH24:MI:SS.US TZH:TZM');

SELECT pg_typeof(TIMETZ '12:34:56+05:30');


-- ============================================================
-- 110. COMPLETE TEMPORAL CAST MATRIX
-- ============================================================

SELECT DATE '2026-07-15'::TIMESTAMP;
SELECT DATE '2026-07-15'::TIMESTAMPTZ;
SELECT DATE '2026-07-15'::TIME;
SELECT DATE '2026-07-15'::TIMETZ;

SELECT TIMESTAMP '2026-07-15 13:45:30'::DATE;
SELECT TIMESTAMP '2026-07-15 13:45:30'::TIME;
SELECT TIMESTAMP '2026-07-15 13:45:30'::TIMETZ;
SELECT TIMESTAMP '2026-07-15 13:45:30'::TIMESTAMPTZ;

SELECT TIMESTAMPTZ '2026-07-15 13:45:30+05:30'::DATE;
SELECT TIMESTAMPTZ '2026-07-15 13:45:30+05:30'::TIME;
SELECT TIMESTAMPTZ '2026-07-15 13:45:30+05:30'::TIMETZ;
SELECT TIMESTAMPTZ '2026-07-15 13:45:30+05:30'::TIMESTAMP;

SELECT TIME '13:45:30'::TIMESTAMP;
SELECT TIME '13:45:30'::TIMETZ;

SELECT TIMETZ '13:45:30+05:30'::TIME;

SELECT INTERVAL '1 day'::TEXT;
SELECT INTERVAL '1 day'::VARCHAR;

SELECT CAST(DATE '2026-07-15' AS TIMESTAMP);
SELECT CAST(DATE '2026-07-15' AS TIMESTAMPTZ);
SELECT CAST(TIMESTAMP '2026-07-15 13:45:30' AS DATE);
SELECT CAST(TIMESTAMP '2026-07-15 13:45:30' AS TIME);
SELECT CAST(TIMESTAMPTZ '2026-07-15 13:45:30+00' AS DATE);
SELECT CAST(TIMESTAMPTZ '2026-07-15 13:45:30+00' AS TIMESTAMP);
SELECT CAST(TIME '13:45:30' AS TIMESTAMP);
SELECT CAST(TIMETZ '13:45:30+05:30' AS TIME);

SELECT CAST(NULL AS DATE);
SELECT CAST(NULL AS TIME);
SELECT CAST(NULL AS TIMESTAMP);
SELECT CAST(NULL AS TIMESTAMPTZ);
SELECT CAST(NULL AS TIMETZ);
SELECT CAST(NULL AS INTERVAL);


-- ============================================================
-- 111. TIME / TIMETZ COMPARISON MATRIX
-- ============================================================

SELECT TIME '10:00:00' = TIME '10:00:00';
SELECT TIME '10:00:00' <> TIME '11:00:00';
SELECT TIME '10:00:00' < TIME '11:00:00';
SELECT TIME '11:00:00' > TIME '10:00:00';
SELECT TIME '10:00:00' <= TIME '10:00:00';
SELECT TIME '10:00:00' >= TIME '10:00:00';

SELECT TIMETZ '10:00:00+00' = TIMETZ '15:30:00+05:30';
SELECT TIMETZ '10:00:00+00' <> TIMETZ '15:00:00+05:30';
SELECT TIMETZ '10:00:00+00' < TIMETZ '11:00:00+00';
SELECT TIMETZ '11:00:00+00' > TIMETZ '10:00:00+00';

SELECT TIME '10:00:00' IS DISTINCT FROM TIME '10:00:00';
SELECT TIME '10:00:00' IS DISTINCT FROM NULL;
SELECT NULL::TIME IS NOT DISTINCT FROM NULL::TIME;

SELECT TIMETZ '10:00:00+00' IS DISTINCT FROM NULL;
SELECT NULL::TIMETZ IS NOT DISTINCT FROM NULL::TIMETZ;


-- ============================================================
-- 112. TEMPORAL OPERATOR MATRIX
-- ============================================================

SELECT DATE '2026-07-15' + 5;
SELECT DATE '2026-07-15' - 5;
SELECT DATE '2026-07-15' - DATE '2026-07-01';

SELECT DATE '2026-07-15' + INTERVAL '2 days';
SELECT DATE '2026-07-15' - INTERVAL '2 days';

SELECT TIMESTAMP '2026-07-15 12:00:00' + INTERVAL '2 hours';
SELECT TIMESTAMP '2026-07-15 12:00:00' - INTERVAL '2 hours';

SELECT TIMESTAMPTZ '2026-07-15 12:00:00+00' + INTERVAL '2 hours';
SELECT TIMESTAMPTZ '2026-07-15 12:00:00+00' - INTERVAL '2 hours';

SELECT TIME '12:00:00' + INTERVAL '2 hours';
SELECT TIME '12:00:00' - INTERVAL '2 hours';

SELECT TIMETZ '12:00:00+00' + INTERVAL '2 hours';
SELECT TIMETZ '12:00:00+00' - INTERVAL '2 hours';

SELECT INTERVAL '2 days' + INTERVAL '3 hours';
SELECT INTERVAL '2 days' - INTERVAL '3 hours';

SELECT INTERVAL '2 days' * 2;
SELECT 2 * INTERVAL '2 days';

SELECT INTERVAL '2 days' / 2;
SELECT INTERVAL '2 hours' * 1.5;
SELECT INTERVAL '2 hours' / 1.5;


-- ============================================================
-- 113. INTERVAL FIELD EXTRACTION
-- ============================================================

SELECT EXTRACT(YEAR FROM INTERVAL '2 years 3 months');
SELECT EXTRACT(MONTH FROM INTERVAL '2 years 3 months');
SELECT EXTRACT(DAY FROM INTERVAL '4 days');
SELECT EXTRACT(HOUR FROM INTERVAL '5 hours');
SELECT EXTRACT(MINUTE FROM INTERVAL '6 minutes');
SELECT EXTRACT(SECOND FROM INTERVAL '7.123456 seconds');

SELECT EXTRACT(MILLISECONDS FROM INTERVAL '7.123456 seconds');
SELECT EXTRACT(MICROSECONDS FROM INTERVAL '7.123456 seconds');

SELECT DATE_PART('year', INTERVAL '2 years');
SELECT DATE_PART('month', INTERVAL '3 months');
SELECT DATE_PART('day', INTERVAL '4 days');
SELECT DATE_PART('hour', INTERVAL '5 hours');
SELECT DATE_PART('minute', INTERVAL '6 minutes');
SELECT DATE_PART('second', INTERVAL '7.123456 seconds');


-- ============================================================
-- 114. INTERVAL ARITHMETIC EDGE CASES
-- ============================================================

SELECT INTERVAL '1 year' + INTERVAL '2 months';
SELECT INTERVAL '1 year' - INTERVAL '2 months';

SELECT INTERVAL '-1 year';
SELECT INTERVAL '-2 months';
SELECT INTERVAL '-3 days';
SELECT INTERVAL '-04:05:06';

SELECT INTERVAL '-1 year 2 months';
SELECT INTERVAL '1 year -2 months';
SELECT INTERVAL '-1 day +02:03:04';

SELECT INTERVAL '0 seconds';
SELECT INTERVAL '0 days';
SELECT INTERVAL '0 months';

SELECT INTERVAL '0.5 seconds';
SELECT INTERVAL '0.123456 seconds';
SELECT INTERVAL '1.123456789 seconds';

SELECT INTERVAL '1000 years';
SELECT INTERVAL '-1000 years';
SELECT INTERVAL '100000 days';
SELECT INTERVAL '-100000 days';

SELECT JUSTIFY_DAYS(INTERVAL '90 days');
SELECT JUSTIFY_HOURS(INTERVAL '50 hours');
SELECT JUSTIFY_INTERVAL(INTERVAL '35 days 50 hours');

SELECT INTERVAL '1 day' * 0;
SELECT INTERVAL '1 day' / 1;
SELECT INTERVAL '1 day' * -1;
SELECT INTERVAL '1 day' / -1;


-- ============================================================
-- 115. GREATEST / LEAST TEMPORAL VALUES
-- ============================================================

SELECT GREATEST(
    DATE '2026-01-01',
    DATE '2026-02-01',
    DATE '2026-03-01'
);

SELECT LEAST(
    DATE '2026-01-01',
    DATE '2026-02-01',
    DATE '2026-03-01'
);

SELECT GREATEST(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-02-01',
    TIMESTAMP '2026-03-01'
);

SELECT LEAST(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-02-01',
    TIMESTAMP '2026-03-01'
);

SELECT GREATEST(
    TIMESTAMPTZ '2026-01-01 00:00:00+00',
    TIMESTAMPTZ '2026-02-01 00:00:00+00'
);

SELECT LEAST(
    TIMESTAMPTZ '2026-01-01 00:00:00+00',
    TIMESTAMPTZ '2026-02-01 00:00:00+00'
);

SELECT GREATEST(
    TIME '10:00:00',
    TIME '12:00:00',
    TIME '14:00:00'
);

SELECT LEAST(
    TIME '10:00:00',
    TIME '12:00:00',
    TIME '14:00:00'
);

SELECT GREATEST(
    INTERVAL '1 day',
    INTERVAL '2 days',
    INTERVAL '3 days'
);

SELECT LEAST(
    INTERVAL '1 day',
    INTERVAL '2 days',
    INTERVAL '3 days'
);


-- ============================================================
-- 116. NULLIF TEMPORAL TYPE RESOLUTION
-- ============================================================

SELECT NULLIF(
    DATE '2026-07-15',
    DATE '2026-07-15'
);

SELECT NULLIF(
    DATE '2026-07-15',
    DATE '2026-07-16'
);

SELECT NULLIF(
    TIMESTAMP '2026-07-15 12:00:00',
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT NULLIF(
    TIMESTAMPTZ '2026-07-15 12:00:00+00',
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
);

SELECT NULLIF(
    TIME '12:00:00',
    TIME '12:00:00'
);

SELECT NULLIF(
    TIMETZ '12:00:00+00',
    TIMETZ '12:00:00+00'
);

SELECT pg_typeof(
    NULLIF(DATE '2026-07-15', DATE '2026-07-16')
);

SELECT pg_typeof(
    NULLIF(
        TIMESTAMP '2026-07-15 12:00:00',
        TIMESTAMP '2026-07-16 12:00:00'
    )
);


-- ============================================================
-- 117. CASE TEMPORAL TYPE RESOLUTION
-- ============================================================

SELECT CASE
    WHEN TRUE THEN DATE '2026-07-15'
    ELSE DATE '2026-07-16'
END;

SELECT CASE
    WHEN TRUE THEN TIMESTAMP '2026-07-15 12:00:00'
    ELSE TIMESTAMP '2026-07-16 12:00:00'
END;

SELECT CASE
    WHEN TRUE THEN TIMESTAMPTZ '2026-07-15 12:00:00+00'
    ELSE TIMESTAMPTZ '2026-07-16 12:00:00+00'
END;

SELECT CASE
    WHEN TRUE THEN TIME '12:00:00'
    ELSE TIME '13:00:00'
END;

SELECT CASE
    WHEN TRUE THEN TIMETZ '12:00:00+00'
    ELSE TIMETZ '13:00:00+00'
END;

SELECT CASE
    WHEN TRUE THEN INTERVAL '1 day'
    ELSE INTERVAL '2 days'
END;

SELECT pg_typeof(
    CASE
        WHEN TRUE THEN DATE '2026-07-15'
        ELSE DATE '2026-07-16'
    END
);


-- ============================================================
-- 118. COALESCE TEMPORAL TYPE RESOLUTION
-- ============================================================

SELECT COALESCE(
    NULL::DATE,
    DATE '2026-07-15'
);

SELECT COALESCE(
    NULL::TIMESTAMP,
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT COALESCE(
    NULL::TIMESTAMPTZ,
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
);

SELECT COALESCE(
    NULL::TIME,
    TIME '12:00:00'
);

SELECT COALESCE(
    NULL::TIMETZ,
    TIMETZ '12:00:00+00'
);

SELECT COALESCE(
    NULL::INTERVAL,
    INTERVAL '1 day'
);


-- ============================================================
-- 119. TEMPORAL VALUES / VALUES CLAUSE
-- ============================================================

SELECT *
FROM (
    VALUES
        (DATE '2026-01-01'),
        (DATE '2026-02-01'),
        (DATE '2026-03-01')
) AS v(d);

SELECT *
FROM (
    VALUES
        (TIMESTAMP '2026-01-01 10:00:00'),
        (TIMESTAMP '2026-02-01 11:00:00'),
        (TIMESTAMP '2026-03-01 12:00:00')
) AS v(ts);

SELECT *
FROM (
    VALUES
        (TIMESTAMPTZ '2026-01-01 10:00:00+00'),
        (TIMESTAMPTZ '2026-02-01 11:00:00+00'),
        (TIMESTAMPTZ '2026-03-01 12:00:00+00')
) AS v(tsz);


-- ============================================================
-- 120. TEMPORAL UNION / INTERSECT / EXCEPT
-- ============================================================

SELECT DATE '2026-01-01'
UNION
SELECT DATE '2026-01-02';

SELECT DATE '2026-01-01'
UNION ALL
SELECT DATE '2026-01-01';

SELECT DATE '2026-01-01'
INTERSECT
SELECT DATE '2026-01-01';

SELECT DATE '2026-01-01'
EXCEPT
SELECT DATE '2026-01-02';

SELECT TIMESTAMP '2026-01-01 00:00:00'
UNION
SELECT TIMESTAMP '2026-01-02 00:00:00';

SELECT TIMESTAMPTZ '2026-01-01 00:00:00+00'
UNION
SELECT TIMESTAMPTZ '2026-01-02 00:00:00+00';


-- ============================================================
-- 121. TEMPORAL BETWEEN / SYMMETRIC
-- ============================================================

SELECT DATE '2026-07-15'
BETWEEN DATE '2026-07-01' AND DATE '2026-07-31';

SELECT DATE '2026-07-15'
NOT BETWEEN DATE '2026-08-01' AND DATE '2026-08-31';

SELECT DATE '2026-07-15'
BETWEEN SYMMETRIC DATE '2026-07-31' AND DATE '2026-07-01';

SELECT TIMESTAMP '2026-07-15 12:00:00'
BETWEEN TIMESTAMP '2026-07-01 00:00:00'
AND TIMESTAMP '2026-07-31 23:59:59';

SELECT TIMESTAMPTZ '2026-07-15 12:00:00+00'
BETWEEN TIMESTAMPTZ '2026-07-01 00:00:00+00'
AND TIMESTAMPTZ '2026-07-31 23:59:59+00';

SELECT TIME '12:00:00'
BETWEEN TIME '10:00:00' AND TIME '14:00:00';


-- ============================================================
-- 122. TEMPORAL ANY / ALL
-- ============================================================

SELECT DATE '2026-07-15' = ANY (
    ARRAY[
        DATE '2026-07-01',
        DATE '2026-07-15',
        DATE '2026-07-31'
    ]
);

SELECT DATE '2026-07-15' = ALL (
    ARRAY[
        DATE '2026-07-15'
    ]
);

SELECT TIMESTAMP '2026-07-15 12:00:00' = ANY (
    ARRAY[
        TIMESTAMP '2026-07-01 12:00:00',
        TIMESTAMP '2026-07-15 12:00:00'
    ]
);

SELECT TIMESTAMPTZ '2026-07-15 12:00:00+00' = ANY (
    ARRAY[
        TIMESTAMPTZ '2026-07-15 12:00:00+00',
        TIMESTAMPTZ '2026-07-16 12:00:00+00'
    ]
);

SELECT TIME '12:00:00' = ANY (
    ARRAY[
        TIME '10:00:00',
        TIME '12:00:00',
        TIME '14:00:00'
    ]
);


-- ============================================================
-- 123. TEMPORAL IN
-- ============================================================

SELECT DATE '2026-07-15' IN (
    DATE '2026-07-01',
    DATE '2026-07-15',
    DATE '2026-07-31'
);

SELECT TIMESTAMP '2026-07-15 12:00:00' IN (
    TIMESTAMP '2026-07-01 12:00:00',
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT TIMESTAMPTZ '2026-07-15 12:00:00+00' IN (
    TIMESTAMPTZ '2026-07-01 12:00:00+00',
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
);

SELECT TIME '12:00:00' IN (
    TIME '10:00:00',
    TIME '12:00:00',
    TIME '14:00:00'
);


-- ============================================================
-- 124. TEMPORAL ARRAYS
-- ============================================================

SELECT ARRAY[
    DATE '2026-01-01',
    DATE '2026-02-01',
    DATE '2026-03-01'
];

SELECT ARRAY[
    TIMESTAMP '2026-01-01 00:00:00',
    TIMESTAMP '2026-02-01 00:00:00'
];

SELECT ARRAY[
    TIMESTAMPTZ '2026-01-01 00:00:00+00',
    TIMESTAMPTZ '2026-02-01 00:00:00+00'
];

SELECT ARRAY[
    TIME '10:00:00',
    TIME '12:00:00'
];

SELECT ARRAY[
    TIMETZ '10:00:00+00',
    TIMETZ '12:00:00+05:30'
];

SELECT ARRAY[
    INTERVAL '1 day',
    INTERVAL '2 days'
];

SELECT ARRAY[
    DATE '2026-01-01',
    NULL::DATE,
    DATE '2026-03-01'
];

SELECT ARRAY_LENGTH(
    ARRAY[
        DATE '2026-01-01',
        DATE '2026-02-01',
        DATE '2026-03-01'
    ],
    1
);


-- ============================================================
-- 125. TEMPORAL ARRAY ACCESS / OPERATORS
-- ============================================================

SELECT (
    ARRAY[
        DATE '2026-01-01',
        DATE '2026-02-01',
        DATE '2026-03-01'
    ]
)[1];

SELECT (
    ARRAY[
        TIMESTAMP '2026-01-01 00:00:00',
        TIMESTAMP '2026-02-01 00:00:00'
    ]
)[2];

SELECT ARRAY_POSITION(
    ARRAY[
        DATE '2026-01-01',
        DATE '2026-02-01',
        DATE '2026-03-01'
    ],
    DATE '2026-02-01'
);

SELECT DATE '2026-02-01' = ANY (
    ARRAY[
        DATE '2026-01-01',
        DATE '2026-02-01'
    ]
);

SELECT ARRAY_TO_STRING(
    ARRAY[
        DATE '2026-01-01',
        DATE '2026-02-01'
    ],
    ','
);


-- ============================================================
-- 126. TEMPORAL CTE / SUBQUERY
-- ============================================================

WITH dates AS (
    SELECT DATE '2026-01-01' AS d
    UNION ALL
    SELECT DATE '2026-02-01'
    UNION ALL
    SELECT DATE '2026-03-01'
)
SELECT
    d,
    d + 1 AS next_day
FROM dates
ORDER BY d;

WITH timestamps AS (
    SELECT TIMESTAMP '2026-01-01 10:00:00' AS ts
)
SELECT
    ts,
    ts + INTERVAL '1 hour'
FROM timestamps;

SELECT *
FROM (
    SELECT
        DATE '2026-07-15' AS d,
        TIMESTAMP '2026-07-15 12:00:00' AS ts,
        TIMESTAMPTZ '2026-07-15 12:00:00+00' AS tsz
) s;


-- ============================================================
-- 127. TEMPORAL WINDOW FUNCTIONS
-- ============================================================

DROP TABLE IF EXISTS plomid_datetime_window_test;

CREATE TEMP TABLE plomid_datetime_window_test (
    id INTEGER,
    event_date DATE,
    event_ts TIMESTAMP,
    event_tsz TIMESTAMPTZ
);

INSERT INTO plomid_datetime_window_test VALUES
(
    1,
    DATE '2026-07-01',
    TIMESTAMP '2026-07-01 10:00:00',
    TIMESTAMPTZ '2026-07-01 10:00:00+00'
),
(
    2,
    DATE '2026-07-02',
    TIMESTAMP '2026-07-02 11:00:00',
    TIMESTAMPTZ '2026-07-02 11:00:00+00'
),
(
    3,
    DATE '2026-07-03',
    TIMESTAMP '2026-07-03 12:00:00',
    TIMESTAMPTZ '2026-07-03 12:00:00+00'
),
(
    4,
    DATE '2026-07-04',
    TIMESTAMP '2026-07-04 13:00:00',
    TIMESTAMPTZ '2026-07-04 13:00:00+00'
);

SELECT
    id,
    event_date,
    LAG(event_date) OVER (ORDER BY event_date),
    LEAD(event_date) OVER (ORDER BY event_date)
FROM plomid_datetime_window_test
ORDER BY id;

SELECT
    id,
    event_ts,
    LAG(event_ts) OVER (ORDER BY event_ts),
    LEAD(event_ts) OVER (ORDER BY event_ts)
FROM plomid_datetime_window_test
ORDER BY id;

SELECT
    id,
    event_ts,
    FIRST_VALUE(event_ts) OVER (ORDER BY event_ts),
    LAST_VALUE(event_ts) OVER (
        ORDER BY event_ts
        ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
    )
FROM plomid_datetime_window_test
ORDER BY id;

SELECT
    id,
    event_date,
    ROW_NUMBER() OVER (ORDER BY event_date),
    RANK() OVER (ORDER BY event_date),
    DENSE_RANK() OVER (ORDER BY event_date)
FROM plomid_datetime_window_test
ORDER BY id;


-- ============================================================
-- 128. TEMPORAL AGGREGATES
-- ============================================================

SELECT MIN(event_date)
FROM plomid_datetime_window_test;

SELECT MAX(event_date)
FROM plomid_datetime_window_test;

SELECT MIN(event_ts)
FROM plomid_datetime_window_test;

SELECT MAX(event_ts)
FROM plomid_datetime_window_test;

SELECT MIN(event_tsz)
FROM plomid_datetime_window_test;

SELECT MAX(event_tsz)
FROM plomid_datetime_window_test;

SELECT COUNT(event_date)
FROM plomid_datetime_window_test;

SELECT COUNT(DISTINCT event_date)
FROM plomid_datetime_window_test;

SELECT
    MIN(event_date),
    MAX(event_date),
    COUNT(*),
    COUNT(DISTINCT event_date)
FROM plomid_datetime_window_test;

SELECT
    event_date,
    COUNT(*)
FROM plomid_datetime_window_test
GROUP BY event_date
HAVING COUNT(*) >= 1
ORDER BY event_date;


-- ============================================================
-- 129. DISTINCT ON TEMPORAL ORDERING
-- ============================================================

SELECT DISTINCT ON (event_date)
    event_date,
    event_ts,
    id
FROM plomid_datetime_window_test
ORDER BY event_date, event_ts DESC;


-- ============================================================
-- 130. FILTER AGGREGATES WITH TEMPORAL PREDICATES
-- ============================================================

SELECT
    COUNT(*) FILTER (
        WHERE event_date >= DATE '2026-07-02'
    ) AS recent_count,
    COUNT(*) FILTER (
        WHERE event_ts >= TIMESTAMP '2026-07-02 00:00:00'
    ) AS timestamp_count
FROM plomid_datetime_window_test;


-- ============================================================
-- 131. TEMPORAL PREDICATES / QUERY CONDITIONS
-- ============================================================

SELECT *
FROM plomid_datetime_window_test
WHERE event_date = DATE '2026-07-02';

SELECT *
FROM plomid_datetime_window_test
WHERE event_date < DATE '2026-07-03';

SELECT *
FROM plomid_datetime_window_test
WHERE event_date <= DATE '2026-07-03';

SELECT *
FROM plomid_datetime_window_test
WHERE event_date > DATE '2026-07-01';

SELECT *
FROM plomid_datetime_window_test
WHERE event_date >= DATE '2026-07-02';

SELECT *
FROM plomid_datetime_window_test
WHERE event_date BETWEEN DATE '2026-07-01'
                     AND DATE '2026-07-03';

SELECT *
FROM plomid_datetime_window_test
WHERE event_ts >= TIMESTAMP '2026-07-02 00:00:00'
  AND event_ts < TIMESTAMP '2026-07-04 00:00:00';

SELECT *
FROM plomid_datetime_window_test
WHERE event_tsz >= TIMESTAMPTZ '2026-07-02 00:00:00+00'
  AND event_tsz < TIMESTAMPTZ '2026-07-04 00:00:00+00';


-- ============================================================
-- 132. TEMPORAL INDEXES
-- ============================================================

CREATE INDEX plomid_datetime_date_idx
ON plomid_datetime_window_test (event_date);

CREATE INDEX plomid_datetime_ts_idx
ON plomid_datetime_window_test (event_ts);

CREATE INDEX plomid_datetime_tsz_idx
ON plomid_datetime_window_test (event_tsz);

SELECT *
FROM plomid_datetime_window_test
WHERE event_date >= DATE '2026-07-02'
  AND event_date < DATE '2026-07-04'
ORDER BY event_date;

SELECT *
FROM plomid_datetime_window_test
WHERE event_ts >= TIMESTAMP '2026-07-02 00:00:00'
  AND event_ts < TIMESTAMP '2026-07-04 00:00:00'
ORDER BY event_ts;

SELECT *
FROM plomid_datetime_window_test
WHERE event_tsz >= TIMESTAMPTZ '2026-07-02 00:00:00+00'
  AND event_tsz < TIMESTAMPTZ '2026-07-04 00:00:00+00'
ORDER BY event_tsz;


-- ============================================================
-- 133. PARTIAL INDEX WITH TEMPORAL PREDICATE
-- ============================================================

CREATE INDEX plomid_datetime_partial_idx
ON plomid_datetime_window_test (event_date)
WHERE event_date >= DATE '2026-07-01';

SELECT *
FROM plomid_datetime_window_test
WHERE event_date >= DATE '2026-07-02';


-- ============================================================
-- 134. DATE_ADD / DATE_SUBTRACT COMPLETE USAGE
-- ============================================================

SELECT DATE_ADD(
    DATE '2026-07-15',
    INTERVAL '1 day'
);

SELECT DATE_ADD(
    DATE '2026-07-15',
    INTERVAL '1 month'
);

SELECT DATE_ADD(
    DATE '2026-07-15',
    INTERVAL '1 year'
);

SELECT DATE_SUBTRACT(
    DATE '2026-07-15',
    INTERVAL '1 day'
);

SELECT DATE_SUBTRACT(
    DATE '2026-07-15',
    INTERVAL '1 month'
);

SELECT DATE_SUBTRACT(
    DATE '2026-07-15',
    INTERVAL '1 year'
);

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-07-15 12:00:00+00',
    INTERVAL '1 day'
);

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-07-15 12:00:00+00',
    INTERVAL '1 day',
    'America/New_York'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-07-15 12:00:00+00',
    INTERVAL '1 day'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-07-15 12:00:00+00',
    INTERVAL '1 day',
    'America/New_York'
);


-- ============================================================
-- 135. MONTH-END ARITHMETIC
-- ============================================================

SELECT DATE '2024-01-31' + INTERVAL '1 month';
SELECT DATE '2024-02-29' + INTERVAL '1 month';
SELECT DATE '2023-01-31' + INTERVAL '1 month';
SELECT DATE '2023-02-28' + INTERVAL '1 month';

SELECT DATE '2024-03-31' - INTERVAL '1 month';
SELECT DATE '2023-03-31' - INTERVAL '1 month';

SELECT DATE_ADD(
    DATE '2024-01-31',
    INTERVAL '1 month'
);

SELECT DATE_ADD(
    DATE '2023-01-31',
    INTERVAL '1 month'
);

SELECT DATE_SUBTRACT(
    DATE '2024-03-31',
    INTERVAL '1 month'
);

SELECT DATE_SUBTRACT(
    DATE '2023-03-31',
    INTERVAL '1 month'
);


-- ============================================================
-- 136. LEAP YEAR / CALENDAR EDGE CASES
-- ============================================================

SELECT DATE '2000-02-29';
SELECT DATE '1900-02-28';
SELECT DATE '2004-02-29';
SELECT DATE '2100-02-28';

SELECT DATE '2000-02-29' + INTERVAL '1 year';
SELECT DATE '2000-02-29' - INTERVAL '1 year';

SELECT DATE '2024-02-29' + INTERVAL '1 year';
SELECT DATE '2024-02-29' - INTERVAL '1 year';

SELECT AGE(
    DATE '2024-02-29',
    DATE '2023-02-28'
);

SELECT AGE(
    DATE '2024-02-29',
    DATE '2020-02-29'
);


-- ============================================================
-- 137. EPOCH EDGE CASES
-- ============================================================

SELECT EXTRACT(
    EPOCH FROM TIMESTAMP '1970-01-01 00:00:00'
);

SELECT EXTRACT(
    EPOCH FROM TIMESTAMP '1969-12-31 23:59:59'
);

SELECT EXTRACT(
    EPOCH FROM TIMESTAMPTZ '1970-01-01 00:00:00+00'
);

SELECT EXTRACT(
    EPOCH FROM TIMESTAMPTZ '1969-12-31 23:59:59+00'
);

SELECT TO_TIMESTAMP(0);
SELECT TO_TIMESTAMP(1);
SELECT TO_TIMESTAMP(-1);
SELECT TO_TIMESTAMP(0.123456);

SELECT TIMESTAMP '1970-01-01 00:00:00'
       + INTERVAL '1 microsecond';

SELECT TIMESTAMP '1970-01-01 00:00:00'
       - INTERVAL '1 microsecond';


-- ============================================================
-- 138. BC / AD ARITHMETIC
-- ============================================================

SELECT DATE '4713-01-01 BC';
SELECT DATE '4713-01-01 BC' + INTERVAL '1 year';
SELECT DATE '4713-01-01 BC' - INTERVAL '1 year';

SELECT TIMESTAMP '0001-01-01 00:00:00 BC';
SELECT TIMESTAMP '0001-01-01 00:00:00 BC'
       + INTERVAL '1 year';

SELECT AGE(
    DATE '0001-01-01 AD',
    DATE '0001-01-01 BC'
);

SELECT TO_CHAR(
    DATE '0001-01-01 BC',
    'YYYY-MM-DD BC'
);


-- ============================================================
-- 139. TIMEZONE() FUNCTION OVERLOADS
-- ============================================================

SELECT TIMEZONE(
    'UTC',
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT TIMEZONE(
    'Asia/Kolkata',
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT TIMEZONE(
    'UTC',
    TIMESTAMPTZ '2026-07-15 12:00:00+05:30'
);

SELECT TIMEZONE(
    'America/New_York',
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
);

SELECT TIMEZONE(
    INTERVAL '+05:30',
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT TIMEZONE(
    INTERVAL '-04:00',
    TIMESTAMP '2026-07-15 12:00:00'
);

SELECT TIMEZONE(
    'UTC',
    TIMETZ '12:00:00+05:30'
);

SELECT TIMEZONE(
    'Asia/Kolkata',
    TIMETZ '12:00:00+00'
);


-- ============================================================
-- 140. AT TIME ZONE COMPLETE MATRIX
-- ============================================================

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'UTC';

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
    AT TIME ZONE 'UTC';

SELECT
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
    AT TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
    AT TIME ZONE 'America/New_York';

SELECT
    TIMETZ '12:00:00+05:30'
    AT TIME ZONE 'UTC';

SELECT
    TIMETZ '12:00:00+05:30'
    AT TIME ZONE 'Asia/Kolkata';

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'Asia/Kolkata'
    AT TIME ZONE 'UTC';

SELECT
    TIMESTAMP '2026-07-15 12:00:00'
    AT TIME ZONE 'America/New_York'
    AT TIME ZONE 'Asia/Kolkata';


-- ============================================================
-- 141. SESSION TIME ZONE BEHAVIOR
-- ============================================================

SHOW TIME ZONE;

SET TIME ZONE 'UTC';

SELECT CURRENT_TIMESTAMP;
SELECT CURRENT_TIME;
SELECT LOCALTIMESTAMP;

SELECT
    TIMESTAMPTZ '2026-07-15 12:00:00+00';

SET TIME ZONE 'Asia/Kolkata';

SELECT CURRENT_TIMESTAMP;
SELECT CURRENT_TIME;
SELECT LOCALTIMESTAMP;

SELECT
    TIMESTAMPTZ '2026-07-15 12:00:00+00';

SET TIME ZONE 'America/New_York';

SELECT CURRENT_TIMESTAMP;
SELECT CURRENT_TIME;
SELECT LOCALTIMESTAMP;

SELECT
    TIMESTAMPTZ '2026-07-15 12:00:00+00';

RESET TIME ZONE;

SHOW TIME ZONE;


-- ============================================================
-- 142. SET TIME ZONE USING OFFSET
-- ============================================================

SET TIME ZONE 'UTC';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE '+05:30';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE '-04:00';

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE INTERVAL '05:30' HOUR TO MINUTE;

SELECT CURRENT_TIMESTAMP;

SET TIME ZONE INTERVAL '-04:00' HOUR TO MINUTE;

SELECT CURRENT_TIMESTAMP;

RESET TIME ZONE;


-- ============================================================
-- 143. TIMEZONE CATALOG INTROSPECTION
-- ============================================================

SELECT
    name,
    abbrev,
    utc_offset,
    is_dst
FROM pg_timezone_names
WHERE name IN (
    'UTC',
    'Asia/Kolkata',
    'America/New_York',
    'Europe/London'
)
ORDER BY name;

SELECT
    abbrev,
    utc_offset,
    is_dst
FROM pg_timezone_abbrevs
WHERE abbrev IN (
    'UTC',
    'GMT',
    'EST',
    'EDT',
    'PST',
    'PDT'
)
ORDER BY abbrev;


-- ============================================================
-- 144. TIMEZONE ABBREVIATION INPUT
-- ============================================================

SELECT TIMESTAMPTZ '2026-07-15 12:00:00 UTC';
SELECT TIMESTAMPTZ '2026-07-15 12:00:00 GMT';
SELECT TIMESTAMPTZ '2026-07-15 12:00:00 EST';
SELECT TIMESTAMPTZ '2026-07-15 12:00:00 EDT';
SELECT TIMESTAMPTZ '2026-07-15 12:00:00 PST';
SELECT TIMESTAMPTZ '2026-07-15 12:00:00 PDT';

SELECT TIMETZ '12:00:00 UTC';
SELECT TIMETZ '12:00:00 EST';
SELECT TIMETZ '12:00:00 EDT';

SELECT TIMESTAMPTZ '2026-07-15 12:00:00 Z';


-- ============================================================
-- 145. DST SPRING-FORWARD TESTS
-- ============================================================

SET TIME ZONE 'America/New_York';

SELECT TIMESTAMPTZ '2026-03-08 01:59:59 America/New_York';
SELECT TIMESTAMPTZ '2026-03-08 03:00:00 America/New_York';

SELECT
    TIMESTAMPTZ '2026-03-08 01:30:00 America/New_York'
    + INTERVAL '1 hour';

SELECT
    TIMESTAMPTZ '2026-03-08 01:30:00 America/New_York'
    + INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-03-08 01:30:00 America/New_York'
    + INTERVAL '24 hours';

RESET TIME ZONE;


-- ============================================================
-- 146. DST FALL-BACK TESTS
-- ============================================================

SET TIME ZONE 'America/New_York';

SELECT TIMESTAMPTZ '2026-11-01 00:59:59 America/New_York';
SELECT TIMESTAMPTZ '2026-11-01 01:30:00 America/New_York';
SELECT TIMESTAMPTZ '2026-11-01 02:00:00 America/New_York';

SELECT
    TIMESTAMPTZ '2026-11-01 01:30:00 America/New_York'
    + INTERVAL '1 hour';

SELECT
    TIMESTAMPTZ '2026-11-01 01:30:00 America/New_York'
    + INTERVAL '1 day';

SELECT
    TIMESTAMPTZ '2026-11-01 01:30:00 America/New_York'
    + INTERVAL '24 hours';

RESET TIME ZONE;


-- ============================================================
-- 147. DATE_ADD / DATE_SUBTRACT ACROSS DST
-- ============================================================

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-03-08 01:30:00 America/New_York',
    INTERVAL '1 day',
    'America/New_York'
);

SELECT DATE_ADD(
    TIMESTAMPTZ '2026-03-08 01:30:00 America/New_York',
    INTERVAL '24 hours',
    'America/New_York'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-11-01 01:30:00 America/New_York',
    INTERVAL '1 day',
    'America/New_York'
);

SELECT DATE_SUBTRACT(
    TIMESTAMPTZ '2026-11-01 01:30:00 America/New_York',
    INTERVAL '24 hours',
    'America/New_York'
);


-- ============================================================
-- 148. GENERATE_SERIES TEMPORAL EDGE CASES
-- ============================================================

SELECT *
FROM generate_series(
    DATE '2026-01-01',
    DATE '2026-01-05',
    INTERVAL '1 day'
);

SELECT *
FROM generate_series(
    DATE '2026-01-05',
    DATE '2026-01-01',
    INTERVAL '-1 day'
);

SELECT *
FROM generate_series(
    DATE '2026-01-01',
    DATE '2026-01-01',
    INTERVAL '1 day'
);

SELECT *
FROM generate_series(
    TIMESTAMP '2026-01-01 00:00:00',
    TIMESTAMP '2026-01-01 03:00:00',
    INTERVAL '1 hour'
);

SELECT *
FROM generate_series(
    TIMESTAMPTZ '2026-01-01 00:00:00+00',
    TIMESTAMPTZ '2026-01-01 03:00:00+00',
    INTERVAL '1 hour'
);

SELECT *
FROM generate_series(
    TIMESTAMP '2026-01-05 00:00:00',
    TIMESTAMP '2026-01-01 00:00:00',
    INTERVAL '-1 day'
);

SELECT *
FROM generate_series(
    DATE '2026-01-01',
    DATE '2026-01-05',
    INTERVAL '2 days'
);


-- ============================================================
-- 149. GENERATE_SERIES DST
-- ============================================================

SET TIME ZONE 'America/New_York';

SELECT *
FROM generate_series(
    TIMESTAMPTZ '2026-03-07 00:00:00 America/New_York',
    TIMESTAMPTZ '2026-03-10 00:00:00 America/New_York',
    INTERVAL '1 day'
);

SELECT *
FROM generate_series(
    TIMESTAMPTZ '2026-10-31 00:00:00 America/New_York',
    TIMESTAMPTZ '2026-11-03 00:00:00 America/New_York',
    INTERVAL '1 day'
);

RESET TIME ZONE;


-- ============================================================
-- 150. DATE_TRUNC COMPLETE FIELD MATRIX
-- ============================================================

SELECT DATE_TRUNC('microseconds',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('milliseconds',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('second',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('minute',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('hour',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('day',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('week',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('month',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('quarter',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('year',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('decade',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('century',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC('millennium',
    TIMESTAMP '2026-07-15 13:45:30.123456');

SELECT DATE_TRUNC(
    'day',
    TIMESTAMPTZ '2026-07-15 13:45:30+00',
    'Asia/Kolkata'
);

SELECT DATE_TRUNC(
    'day',
    TIMESTAMPTZ '2026-07-15 13:45:30+00',
    'America/New_York'
);


-- ============================================================
-- 151. DATE_BIN EDGE CASES
-- ============================================================

SELECT DATE_BIN(
    INTERVAL '15 minutes',
    TIMESTAMP '2026-07-15 13:47:00',
    TIMESTAMP '2026-07-15 00:00:00'
);

SELECT DATE_BIN(
    INTERVAL '1 hour',
    TIMESTAMP '2026-07-15 13:47:00',
    TIMESTAMP '2026-07-15 00:00:00'
);

SELECT DATE_BIN(
    INTERVAL '1 day',
    TIMESTAMP '2026-07-15 13:47:00',
    TIMESTAMP '2026-07-01 00:00:00'
);

SELECT DATE_BIN(
    INTERVAL '15 minutes',
    TIMESTAMPTZ '2026-07-15 13:47:00+00',
    TIMESTAMPTZ '2026-07-15 00:00:00+00'
);

SELECT DATE_BIN(
    INTERVAL '1 hour',
    TIMESTAMPTZ '2026-07-15 13:47:00+00',
    TIMESTAMPTZ '2026-07-15 00:00:00+00'
);


-- ============================================================
-- 152. OVERLAPS COMPLETE TEMPORAL COVERAGE
-- ============================================================

SELECT
    (DATE '2026-01-01', DATE '2026-01-10')
    OVERLAPS
    (DATE '2026-01-05', DATE '2026-01-15');

SELECT
    (DATE '2026-01-01', DATE '2026-01-05')
    OVERLAPS
    (DATE '2026-01-05', DATE '2026-01-10');

SELECT
    (TIMESTAMP '2026-01-01 10:00:00',
     TIMESTAMP '2026-01-01 12:00:00')
    OVERLAPS
    (TIMESTAMP '2026-01-01 11:00:00',
     TIMESTAMP '2026-01-01 13:00:00');

SELECT
    (TIMESTAMPTZ '2026-01-01 10:00:00+00',
     TIMESTAMPTZ '2026-01-01 12:00:00+00')
    OVERLAPS
    (TIMESTAMPTZ '2026-01-01 11:00:00+00',
     TIMESTAMPTZ '2026-01-01 13:00:00+00');

SELECT
    (TIME '10:00:00', TIME '12:00:00')
    OVERLAPS
    (TIME '11:00:00', TIME '13:00:00');

SELECT
    (DATE '2026-01-01', INTERVAL '10 days')
    OVERLAPS
    (DATE '2026-01-05', INTERVAL '10 days');

SELECT
    (TIMESTAMP '2026-01-01 10:00:00', INTERVAL '2 hours')
    OVERLAPS
    (TIMESTAMP '2026-01-01 11:00:00', INTERVAL '2 hours');


-- ============================================================
-- 153. TO_CHAR ADDITIONAL DATETIME FORMATS
-- ============================================================

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'YYYY YYY YY Y'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'IYYY IYY IY IW ID IDDD'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'MM MON MONTH RM'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'DD DDD DY DAY'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30.123456',
    'HH HH12 HH24 MI SS MS US'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30',
    'AM PM A.M. P.M.'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30',
    'Q WW W'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30',
    'SSSS SSSSS'
);

SELECT TO_CHAR(
    TIMESTAMPTZ '2026-07-15 13:45:30+05:30',
    'TZ TZH TZM OF'
);

SELECT TO_CHAR(
    DATE '0001-01-01 BC',
    'YYYY-MM-DD BC'
);

SELECT TO_CHAR(
    DATE '2026-07-15',
    'FMYYYY-FMMM-FMDD'
);

SELECT TO_CHAR(
    TIMESTAMP '2026-07-15 13:45:30',
    'YYYY"YEAR"MM"MONTH"DD'
);


-- ============================================================
-- 154. TO_CHAR INTERVAL FORMATS
-- ============================================================

SELECT TO_CHAR(
    INTERVAL '2 years 3 months 4 days 05:06:07',
    'HH24:MI:SS'
);

SELECT TO_CHAR(
    INTERVAL '2 years 3 months 4 days',
    'YYYY "years" MM "months" DD "days"'
);


-- ============================================================
-- 155. PREPARED STATEMENT TEMPORAL PARAMETERS
-- ============================================================

PREPARE plomid_date_test(DATE) AS
SELECT $1, $1 + 1;

EXECUTE plomid_date_test(DATE '2026-07-15');

PREPARE plomid_timestamp_test(TIMESTAMP) AS
SELECT $1, $1 + INTERVAL '1 hour';

EXECUTE plomid_timestamp_test(
    TIMESTAMP '2026-07-15 12:00:00'
);

PREPARE plomid_timestamptz_test(TIMESTAMPTZ) AS
SELECT $1, $1 + INTERVAL '1 hour';

EXECUTE plomid_timestamptz_test(
    TIMESTAMPTZ '2026-07-15 12:00:00+00'
);

PREPARE plomid_time_test(TIME) AS
SELECT $1, $1 + INTERVAL '1 hour';

EXECUTE plomid_time_test(
    TIME '12:00:00'
);

PREPARE plomid_timetz_test(TIMETZ) AS
SELECT $1, $1 + INTERVAL '1 hour';

EXECUTE plomid_timetz_test(
    TIMETZ '12:00:00+05:30'
);

PREPARE plomid_interval_test(INTERVAL) AS
SELECT $1, $1 * 2;

EXECUTE plomid_interval_test(
    INTERVAL '2 days'
);

DEALLOCATE plomid_date_test;
DEALLOCATE plomid_timestamp_test;
DEALLOCATE plomid_timestamptz_test;
DEALLOCATE plomid_time_test;
DEALLOCATE plomid_timetz_test;
DEALLOCATE plomid_interval_test;


-- ============================================================
-- 156. TEMPORAL NULL PROPAGATION
-- ============================================================

SELECT DATE_TRUNC(
    'day',
    NULL::TIMESTAMP
);

SELECT DATE_TRUNC(
    'day',
    NULL::TIMESTAMPTZ
);

SELECT DATE_BIN(
    INTERVAL '1 hour',
    NULL::TIMESTAMP,
    TIMESTAMP '2026-01-01'
);

SELECT AGE(
    NULL::TIMESTAMP,
    TIMESTAMP '2026-01-01'
);

SELECT AGE(
    TIMESTAMP '2026-01-01',
    NULL::TIMESTAMP
);

SELECT DATE_ADD(
    NULL::DATE,
    INTERVAL '1 day'
);

SELECT DATE_SUBTRACT(
    NULL::DATE,
    INTERVAL '1 day'
);

SELECT MAKE_DATE(
    2026,
    NULL,
    1
);

SELECT MAKE_TIME(
    12,
    NULL,
    0
);

SELECT MAKE_INTERVAL(
    years => NULL,
    months => 1
);

SELECT TO_CHAR(
    NULL::TIMESTAMP,
    'YYYY-MM-DD'
);


-- ============================================================
-- 157. PG_TYPEOF COMPLETE TEMPORAL FUNCTION CHECK
-- ============================================================

SELECT pg_typeof(CURRENT_DATE);
SELECT pg_typeof(CURRENT_TIME);
SELECT pg_typeof(CURRENT_TIMESTAMP);
SELECT pg_typeof(LOCALTIME);
SELECT pg_typeof(LOCALTIMESTAMP);

SELECT pg_typeof(NOW());
SELECT pg_typeof(CLOCK_TIMESTAMP());
SELECT pg_typeof(STATEMENT_TIMESTAMP());
SELECT pg_typeof(TRANSACTION_TIMESTAMP());

SELECT pg_typeof(DATE_TRUNC(
    'day',
    TIMESTAMP '2026-07-15 12:00:00'
));

SELECT pg_typeof(DATE_BIN(
    INTERVAL '1 hour',
    TIMESTAMP '2026-07-15 12:00:00',
    TIMESTAMP '2026-07-15 00:00:00'
));

SELECT pg_typeof(AGE(
    TIMESTAMP '2026-07-15',
    TIMESTAMP '2026-07-01'
));

SELECT pg_typeof(MAKE_DATE(2026, 7, 15));
SELECT pg_typeof(MAKE_TIME(12, 30, 0));
SELECT pg_typeof(MAKE_TIMESTAMP(2026, 7, 15, 12, 30, 0));
SELECT pg_typeof(MAKE_TIMESTAMPTZ(2026, 7, 15, 12, 30, 0));


-- ============================================================
-- 158. INFORMATION_SCHEMA TEMPORAL METADATA
-- ============================================================

SELECT
    table_schema,
    table_name,
    column_name,
    data_type,
    datetime_precision
FROM information_schema.columns
WHERE table_name = 'plomid_datetime_window_test'
ORDER BY ordinal_position;


-- ============================================================
-- 159. TEMPORAL DISTINCT / ORDERING
-- ============================================================

SELECT DISTINCT event_date
FROM plomid_datetime_window_test
ORDER BY event_date;

SELECT DISTINCT event_ts
FROM plomid_datetime_window_test
ORDER BY event_ts;

SELECT DISTINCT event_tsz
FROM plomid_datetime_window_test
ORDER BY event_tsz;

SELECT event_date
FROM plomid_datetime_window_test
ORDER BY event_date ASC;

SELECT event_date
FROM plomid_datetime_window_test
ORDER BY event_date DESC;


-- ============================================================
-- 160. FINAL TEMPORAL CLEANUP
-- ============================================================

DROP TABLE IF EXISTS plomid_datetime_window_test;

DROP SCHEMA IF EXISTS plomid_datetime_test CASCADE;


-- ============================================================
-- 161. VERIFY CLEANUP
-- ============================================================

SELECT COUNT(*)
FROM information_schema.tables
WHERE table_schema = 'plomid_datetime_test';

\echo ''
\echo '============================================================'
\echo 'PLOMID DATE/TIME COMPATIBILITY TEST COMPLETE'
\echo '============================================================'