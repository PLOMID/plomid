-- ============================================
-- JSON OBJECT AGGREGATE DB PROOF
-- ============================================

-- Clean up any previous permanent/temp table
DROP TABLE IF EXISTS public.json_agg_test;
DROP TABLE IF EXISTS json_agg_test;

-- Basic object aggregation
SELECT json_object_agg(key, value)
FROM (
    SELECT 'name' AS key, 'PLOMID' AS value
    UNION ALL
    SELECT 'version', '1'
) x;

SELECT jsonb_object_agg(key, value)
FROM (
    SELECT 'name' AS key, 'PLOMID' AS value
    UNION ALL
    SELECT 'version', '1'
) x;

-- Empty input
SELECT json_object_agg(key, value)
FROM (
    SELECT 'name' AS key, 'PLOMID' AS value
    WHERE FALSE
) x;

SELECT jsonb_object_agg(key, value)
FROM (
    SELECT 'name' AS key, 'PLOMID' AS value
    WHERE FALSE
) x;

-- NULL values
SELECT json_object_agg(key, value)
FROM (
    SELECT 'name' AS key, 'PLOMID' AS value
    UNION ALL
    SELECT 'missing', NULL
) x;

SELECT jsonb_object_agg(key, value)
FROM (
    SELECT 'name' AS key, 'PLOMID' AS value
    UNION ALL
    SELECT 'missing', NULL
) x;

-- ============================================
-- Real table data
-- ============================================

CREATE TEMP TABLE json_agg_test (
    id INTEGER,
    name TEXT,
    active BOOLEAN
);

INSERT INTO json_agg_test VALUES
    (1, 'PLOMID', true),
    (2, 'TEST', false),
    (3, 'DEMO', true);

-- Object aggregation using table data
SELECT json_object_agg(id::text, name)
FROM json_agg_test;

SELECT jsonb_object_agg(id::text, name)
FROM json_agg_test;

-- JSONB values
SELECT jsonb_object_agg(
    id::text,
    jsonb_build_object(
        'name', name,
        'active', active
    )
)
FROM json_agg_test;

-- ============================================
-- Empty table
-- ============================================

TRUNCATE json_agg_test;

SELECT json_object_agg(id::text, name)
FROM json_agg_test;

SELECT jsonb_object_agg(id::text, name)
FROM json_agg_test;

-- ============================================
-- Cleanup
-- ============================================

DROP TABLE IF EXISTS json_agg_test;
