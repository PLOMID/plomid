-- ============================================================
-- REGRESSION: || operator with non-text operands
-- Fixes the "invalid argument for function ||" failures where
-- one operand is text and the other is numeric (e.g. bigint
-- from generate_series). PostgreSQL resolves these via an
-- implicit cast to text.
-- ============================================================

-- 1. text || bigint (the exact failing case: generate_series returns bigint)
SELECT 'Customer ' || gs AS name FROM generate_series(1,3) gs;

-- 2. text || integer
SELECT 'Customer ' || 5::integer AS name;

-- 3. integer || text (reverse order)
SELECT 5::integer || 'Customer ' AS name;

-- 4. text || smallint
SELECT 'n=' || 7::smallint AS name;

-- 5. text || numeric
SELECT 'v=' || 3.14::numeric AS name;

-- 6. text || text (regression: must still work)
SELECT 'a' || 'b'::text AS ab;

-- 7. jsonb || jsonb object merge (regression: must still work)
SELECT '{"a":1}'::jsonb || '{"b":2}'::jsonb AS merged;

-- 8. jsonb || jsonb array concat (regression: must still work)
SELECT '[1,2]'::jsonb || '[3,4]'::jsonb AS concat_arr;

-- 9. jsonb || jsonb scalar concat (regression: must still work)
SELECT '{"a":1}'::jsonb || '2'::jsonb AS obj_scalar;

-- 10. jsonb || jsonb scalar concat reverse (regression: must still work)
SELECT '2'::jsonb || '{"a":1}'::jsonb AS scalar_obj;

-- 11. NULL || text (regression: must return NULL)
SELECT NULL::text || 'x' AS null_left;

-- 12. text || NULL (regression: must return NULL)
SELECT 'x' || NULL::text AS null_right;
