-- ============================================================
-- PLOMID / POSTGRESQL 17 BROAD COMPATIBILITY TEST SUITE
-- ============================================================
--
-- Reference: PostgreSQL 17
--
-- Purpose:
--   Broad SQL / PostgreSQL behavioral compatibility testing.
--
-- IMPORTANT:
--   This is one self-contained SQL file.
--   It intentionally exercises successful statements and
--   expected-error behavior.
--
-- Run with:
--   psql -v ON_ERROR_STOP=off -f postgresql17_compat.sql
--
-- ============================================================

\set ON_ERROR_STOP off
\set VERBOSITY terse

DROP SCHEMA IF EXISTS pg17_compat CASCADE;
CREATE SCHEMA pg17_compat;

SET search_path TO pg17_compat, public;

SELECT version();

SELECT current_database();
SELECT current_schema();
SELECT current_user;
SELECT current_setting('server_version');

-- ============================================================
-- 001. BASIC LITERALS
-- ============================================================

SELECT 0;
SELECT 1;
SELECT -1;
SELECT 2147483647;
SELECT -2147483648;
SELECT 9223372036854775807::bigint;
SELECT -9223372036854775808::bigint;

SELECT 1.0;
SELECT 1.234567890123456789::numeric;
SELECT 1e10::numeric;
SELECT 1e-10::numeric;

SELECT true;
SELECT false;
SELECT NULL;

SELECT 'hello';
SELECT '';
SELECT 'hello world';
SELECT E'hello\nworld';
SELECT E'hello\tworld';
SELECT E'quote: \'hello\'';
SELECT E'backslash: \\';

-- ============================================================
-- 002. NULL SEMANTICS
-- ============================================================

SELECT NULL IS NULL;
SELECT NULL IS NOT NULL;

SELECT 1 IS NULL;
SELECT 1 IS NOT NULL;

SELECT NULL = NULL;
SELECT NULL <> NULL;
SELECT NULL < 1;
SELECT NULL > 1;

SELECT COALESCE(NULL, 1);
SELECT COALESCE(NULL, NULL, 2);

SELECT NULLIF(1, 1);
SELECT NULLIF(1, 2);

SELECT CASE
    WHEN NULL THEN 'true'
    ELSE 'false'
END;

SELECT CASE
    WHEN 1 = 1 THEN 'yes'
    ELSE 'no'
END;

-- ============================================================
-- 003. BOOLEAN LOGIC
-- ============================================================

SELECT true AND true;
SELECT true AND false;
SELECT false AND true;
SELECT false AND false;

SELECT true OR true;
SELECT true OR false;
SELECT false OR false;

SELECT NOT true;
SELECT NOT false;

SELECT true AND NULL;
SELECT false AND NULL;
SELECT true OR NULL;
SELECT false OR NULL;

-- ============================================================
-- 004. NUMERIC TYPES
-- ============================================================

CREATE TABLE numeric_test (
    small smallint,
    integer_value integer,
    big bigint,
    numeric_value numeric,
    decimal_value decimal,
    real_value real,
    double_value double precision
);

INSERT INTO numeric_test VALUES (
    1,
    100,
    10000000000,
    123456789.123456789,
    999999999.999999,
    1.25,
    123456.789
);

SELECT * FROM numeric_test;

SELECT 1 + 2;
SELECT 10 - 3;
SELECT 10 * 3;
SELECT 10 / 3;
SELECT 10 % 3;
SELECT 10 ^ 3;

SELECT abs(-10);
SELECT ceil(1.2);
SELECT floor(1.8);
SELECT round(1.2345, 2);
SELECT trunc(1.2345, 2);

SELECT power(2, 10);
SELECT sqrt(16);
SELECT sign(-10);
SELECT greatest(1,2,3);
SELECT least(1,2,3);

SELECT 1::smallint;
SELECT 1::integer;
SELECT 1::bigint;
SELECT 1::numeric;
SELECT 1::real;
SELECT 1::double precision;

-- ============================================================
-- 005. NUMERIC ERROR BEHAVIOR
-- ============================================================

SELECT 1 / 0;
SELECT 1.0 / 0.0;

SELECT 32767::smallint + 1;
SELECT 2147483647::integer + 1;
SELECT 9223372036854775807::bigint + 1;

-- ============================================================
-- 006. STRING TYPES
-- ============================================================

CREATE TABLE string_test (
    id integer,
    v_char char(10),
    v_varchar varchar(100),
    v_text text
);

INSERT INTO string_test VALUES
(1, 'abc', 'hello', 'PLOMID PostgreSQL'),
(2, 'x', 'world', 'database'),
(3, '', '', '');

SELECT * FROM string_test;

SELECT length('hello');
SELECT char_length('hello');
SELECT octet_length('hello');

SELECT lower('HELLO');
SELECT upper('hello');
SELECT initcap('hello world');

SELECT substring('abcdef' FROM 2 FOR 3);
SELECT left('abcdef', 3);
SELECT right('abcdef', 3);

SELECT ltrim('  hello');
SELECT rtrim('hello  ');
SELECT trim('  hello  ');

SELECT replace('hello world', 'world', 'PLOMID');
SELECT reverse('abc');

SELECT repeat('x', 5);
SELECT concat('a','b','c');
SELECT concat_ws('-', 'a','b','c');

SELECT position('bc' IN 'abcd');
SELECT strpos('abcd', 'bc');

SELECT 'abc' || 'def';

-- ============================================================
-- 007. STRING PATTERN MATCHING
-- ============================================================

SELECT 'hello' LIKE 'hello';
SELECT 'hello' LIKE 'h%';
SELECT 'hello' LIKE '%llo';
SELECT 'hello' LIKE '_ello';

SELECT 'HELLO' ILIKE 'hello';
SELECT 'Hello World' ILIKE '%world%';

SELECT 'abc' SIMILAR TO 'a.c';
SELECT 'abc' SIMILAR TO 'a%';

SELECT regexp_match('hello123', '[0-9]+');
SELECT regexp_matches(
    'a1 b2 c3',
    '[a-z][0-9]',
    'g'
);

SELECT regexp_replace(
    'hello123',
    '[0-9]+',
    'X'
);

SELECT regexp_split_to_array(
    'a,b,c',
    ','
);

SELECT regexp_split_to_table(
    'a,b,c',
    ','
);

-- ============================================================
-- 008. UNICODE
-- ============================================================

SELECT 'José';
SELECT 'साईनाथ';
SELECT ' హైదరాబాద్ ';
SELECT 'こんにちは世界';
SELECT '🚀';
SELECT '👨‍💻🚀🔥';

SELECT length('こんにちは');
SELECT char_length('こんにちは');

-- ============================================================
-- 009. DATE
-- ============================================================

SELECT DATE '2026-01-01';
SELECT DATE '2000-02-29';
SELECT DATE '2024-02-29';

SELECT CURRENT_DATE;

SELECT DATE '2026-01-01' + 1;
SELECT DATE '2026-01-01' - 1;

SELECT DATE '2026-01-10' - DATE '2026-01-01';

SELECT extract(year FROM DATE '2026-01-01');
SELECT extract(month FROM DATE '2026-01-01');
SELECT extract(day FROM DATE '2026-01-01');

SELECT date_trunc('month', DATE '2026-05-15');
SELECT date_trunc('year', DATE '2026-05-15');

-- ============================================================
-- 010. TIME
-- ============================================================

SELECT TIME '12:30:45';
SELECT TIME '23:59:59';

SELECT CURRENT_TIME;

SELECT TIME '12:00:00' + INTERVAL '1 hour';
SELECT TIME '12:00:00' - INTERVAL '1 hour';

SELECT extract(hour FROM TIME '12:30:45');
SELECT extract(minute FROM TIME '12:30:45');
SELECT extract(second FROM TIME '12:30:45');

-- ============================================================
-- 011. TIMESTAMP
-- ============================================================

SELECT TIMESTAMP '2026-01-01 12:30:45';
SELECT TIMESTAMP '2026-01-01 12:30:45' + INTERVAL '1 day';
SELECT TIMESTAMP '2026-01-01 12:30:45' - INTERVAL '1 hour';

SELECT CURRENT_TIMESTAMP;
SELECT LOCALTIMESTAMP;

SELECT date_trunc(
    'day',
    TIMESTAMP '2026-05-15 12:34:56'
);

SELECT extract(
    year FROM TIMESTAMP '2026-05-15 12:34:56'
);

SELECT extract(
    epoch FROM TIMESTAMP '2026-05-15 12:34:56'
);

-- ============================================================
-- 012. TIMESTAMPTZ
-- ============================================================

SELECT TIMESTAMPTZ '2026-01-01 12:30:45+05:30';
SELECT TIMESTAMPTZ '2026-01-01 07:00:00+00';

SELECT CURRENT_TIMESTAMP AT TIME ZONE 'UTC';

SET TIME ZONE 'UTC';

SELECT TIMESTAMPTZ '2026-01-01 12:30:45+05:30';

SET TIME ZONE 'Asia/Kolkata';

SELECT TIMESTAMPTZ '2026-01-01 12:30:45+05:30';

-- ============================================================
-- 013. INTERVAL
-- ============================================================

SELECT INTERVAL '1 day';
SELECT INTERVAL '1 hour';
SELECT INTERVAL '1 minute';
SELECT INTERVAL '1 second';

SELECT INTERVAL '1 year 2 months 3 days';
SELECT INTERVAL '2 hours 30 minutes';

SELECT INTERVAL '1 day' + INTERVAL '2 days';
SELECT INTERVAL '5 days' - INTERVAL '2 days';

SELECT TIMESTAMP '2026-01-01' + INTERVAL '1 month';
SELECT TIMESTAMP '2026-01-01' + INTERVAL '1 year';

-- ============================================================
-- 014. CASTING
-- ============================================================

SELECT '123'::integer;
SELECT '123'::bigint;
SELECT '123.45'::numeric;
SELECT 'true'::boolean;
SELECT '2026-01-01'::date;
SELECT '12:30:00'::time;
SELECT '2026-01-01 12:30:00'::timestamp;

SELECT 123::text;
SELECT true::text;
SELECT DATE '2026-01-01'::text;

-- ============================================================
-- 015. CASE / COALESCE / NULLIF
-- ============================================================

SELECT
    CASE
        WHEN 1 > 2 THEN 'A'
        WHEN 2 > 1 THEN 'B'
        ELSE 'C'
    END;

SELECT
    CASE 2
        WHEN 1 THEN 'A'
        WHEN 2 THEN 'B'
        ELSE 'C'
    END;

SELECT COALESCE(NULL, NULL, 'value');
SELECT NULLIF('a', 'a');
SELECT NULLIF('a', 'b');

-- ============================================================
-- 016. BASIC TABLE / DML
-- ============================================================

CREATE TABLE employees (
    id integer PRIMARY KEY,
    name text NOT NULL,
    department text,
    salary numeric(12,2),
    active boolean DEFAULT true,
    created_at timestamp DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO employees
(id, name, department, salary)
VALUES
(1, 'Alice', 'Engineering', 100000),
(2, 'Bob', 'Engineering', 90000),
(3, 'Carol', 'Finance', 85000),
(4, 'David', 'HR', 70000);

SELECT * FROM employees ORDER BY id;

SELECT id, name
FROM employees
WHERE active
ORDER BY id;

UPDATE employees
SET salary = salary + 5000
WHERE id = 1;

SELECT * FROM employees WHERE id = 1;

DELETE FROM employees
WHERE id = 4;

SELECT * FROM employees ORDER BY id;

-- ============================================================
-- 017. INSERT RETURNING
-- ============================================================

INSERT INTO employees
(id, name, department, salary)
VALUES
(5, 'Eve', 'Engineering', 95000)
RETURNING *;

-- ============================================================
-- 018. UPDATE RETURNING
-- ============================================================

UPDATE employees
SET salary = salary + 1000
WHERE id = 2
RETURNING id, name, salary;

-- ============================================================
-- 019. DELETE RETURNING
-- ============================================================

DELETE FROM employees
WHERE id = 5
RETURNING *;

-- ============================================================
-- 020. UPSERT
-- ============================================================

INSERT INTO employees
(id, name, department, salary)
VALUES
(1, 'Alice Updated', 'Engineering', 120000)
ON CONFLICT (id)
DO UPDATE SET
    name = EXCLUDED.name,
    salary = EXCLUDED.salary
RETURNING *;

-- ============================================================
-- 021. DISTINCT
-- ============================================================

SELECT DISTINCT department
FROM employees
ORDER BY department;

SELECT DISTINCT department, active
FROM employees
ORDER BY department, active;

-- ============================================================
-- 022. GROUP BY
-- ============================================================

SELECT
    department,
    COUNT(*),
    SUM(salary),
    AVG(salary),
    MIN(salary),
    MAX(salary)
FROM employees
GROUP BY department
ORDER BY department;

SELECT
    department,
    COUNT(*)
FROM employees
GROUP BY department
HAVING COUNT(*) >= 1
ORDER BY department;

-- ============================================================
-- 023. GROUPING SETS
-- ============================================================

SELECT
    department,
    active,
    COUNT(*)
FROM employees
GROUP BY GROUPING SETS (
    (department),
    (active),
    ()
)
ORDER BY department NULLS FIRST, active NULLS FIRST;

SELECT
    department,
    active,
    COUNT(*)
FROM employees
GROUP BY ROLLUP (department, active);

SELECT
    department,
    active,
    COUNT(*)
FROM employees
GROUP BY CUBE (department, active);

-- ============================================================
-- 024. ORDER BY / NULLS
-- ============================================================

SELECT * FROM employees
ORDER BY salary ASC;

SELECT * FROM employees
ORDER BY salary DESC;

SELECT * FROM employees
ORDER BY department NULLS FIRST;

SELECT * FROM employees
ORDER BY department NULLS LAST;

-- ============================================================
-- 025. LIMIT / OFFSET
-- ============================================================

SELECT * FROM employees
ORDER BY id
LIMIT 2;

SELECT * FROM employees
ORDER BY id
LIMIT 2 OFFSET 1;

-- ============================================================
-- 026. BETWEEN / IN
-- ============================================================

SELECT *
FROM employees
WHERE salary BETWEEN 80000 AND 110000
ORDER BY id;

SELECT *
FROM employees
WHERE department IN ('Engineering', 'Finance')
ORDER BY id;

SELECT *
FROM employees
WHERE department NOT IN ('HR')
ORDER BY id;

-- ============================================================
-- 027. JOINS
-- ============================================================

CREATE TABLE departments (
    id integer PRIMARY KEY,
    name text UNIQUE
);

INSERT INTO departments VALUES
(1, 'Engineering'),
(2, 'Finance'),
(3, 'HR'),
(4, 'Sales');

CREATE TABLE employee_department (
    employee_id integer,
    department_id integer
);

INSERT INTO employee_department VALUES
(1,1),
(2,1),
(3,2);

SELECT
    e.name,
    d.name
FROM employees e
JOIN employee_department ed
    ON ed.employee_id = e.id
JOIN departments d
    ON d.id = ed.department_id
ORDER BY e.id;

SELECT
    e.name,
    d.name
FROM employees e
LEFT JOIN employee_department ed
    ON ed.employee_id = e.id
LEFT JOIN departments d
    ON d.id = ed.department_id
ORDER BY e.id;

SELECT
    e.name,
    d.name
FROM employees e
RIGHT JOIN employee_department ed
    ON ed.employee_id = e.id
RIGHT JOIN departments d
    ON d.id = ed.department_id
ORDER BY d.id;

SELECT
    e.name,
    d.name
FROM employees e
FULL OUTER JOIN employee_department ed
    ON ed.employee_id = e.id
FULL OUTER JOIN departments d
    ON d.id = ed.department_id;

-- ============================================================
-- 028. CROSS JOIN
-- ============================================================

SELECT *
FROM
    (VALUES (1),(2)) a(x)
CROSS JOIN
    (VALUES ('a'),('b')) b(y)
ORDER BY x, y;

-- ============================================================
-- 029. SELF JOIN
-- ============================================================

CREATE TABLE hierarchy (
    id integer PRIMARY KEY,
    name text,
    parent_id integer
);

INSERT INTO hierarchy VALUES
(1,'CEO',NULL),
(2,'Engineering',1),
(3,'Finance',1),
(4,'Backend',2);

SELECT
    c.name AS child,
    p.name AS parent
FROM hierarchy c
LEFT JOIN hierarchy p
    ON p.id = c.parent_id
ORDER BY c.id;

-- ============================================================
-- 030. SUBQUERIES
-- ============================================================

SELECT *
FROM employees
WHERE salary >
(
    SELECT AVG(salary)
    FROM employees
)
ORDER BY id;

SELECT *
FROM employees e
WHERE EXISTS (
    SELECT 1
    FROM departments d
    WHERE d.name = e.department
)
ORDER BY e.id;

SELECT *
FROM employees e
WHERE NOT EXISTS (
    SELECT 1
    FROM departments d
    WHERE d.name = 'Unknown'
);

-- ============================================================
-- 031. CTE
-- ============================================================

WITH high_paid AS (
    SELECT *
    FROM employees
    WHERE salary > 90000
)
SELECT *
FROM high_paid
ORDER BY id;

WITH totals AS (
    SELECT department, SUM(salary) total
    FROM employees
    GROUP BY department
)
SELECT *
FROM totals
ORDER BY department;

-- ============================================================
-- 032. RECURSIVE CTE
-- ============================================================

WITH RECURSIVE nums(n) AS (
    SELECT 1
    UNION ALL
    SELECT n + 1
    FROM nums
    WHERE n < 10
)
SELECT *
FROM nums
ORDER BY n;

WITH RECURSIVE tree AS (
    SELECT
        id,
        name,
        parent_id,
        0 AS depth
    FROM hierarchy
    WHERE parent_id IS NULL

    UNION ALL

    SELECT
        h.id,
        h.name,
        h.parent_id,
        t.depth + 1
    FROM hierarchy h
    JOIN tree t
      ON h.parent_id = t.id
)
SELECT *
FROM tree
ORDER BY depth, id;

-- ============================================================
-- 033. VALUES
-- ============================================================

SELECT *
FROM (
    VALUES
        (1,'a'),
        (2,'b'),
        (3,'c')
) AS v(id,name);

-- ============================================================
-- 034. SET OPERATIONS
-- ============================================================

SELECT 1
UNION
SELECT 2
UNION
SELECT 2;

SELECT 1
UNION ALL
SELECT 1;

SELECT 1
INTERSECT
SELECT 1;

SELECT 1
INTERSECT
SELECT 2;

SELECT 1
EXCEPT
SELECT 2;

SELECT 1
EXCEPT
SELECT 1;

-- ============================================================
-- 035. WINDOW FUNCTIONS
-- ============================================================

SELECT
    id,
    name,
    salary,
    row_number() OVER (ORDER BY salary DESC) AS rn,
    rank() OVER (ORDER BY salary DESC) AS rnk,
    dense_rank() OVER (ORDER BY salary DESC) AS drnk
FROM employees
ORDER BY id;

SELECT
    id,
    department,
    salary,
    avg(salary) OVER (
        PARTITION BY department
    ) AS department_avg
FROM employees
ORDER BY id;

SELECT
    id,
    salary,
    lag(salary) OVER (ORDER BY id),
    lead(salary) OVER (ORDER BY id)
FROM employees
ORDER BY id;

SELECT
    id,
    salary,
    sum(salary) OVER (
        ORDER BY id
        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
    )
FROM employees
ORDER BY id;

-- ============================================================
-- 036. AGGREGATES
-- ============================================================

SELECT COUNT(*) FROM employees;
SELECT COUNT(name) FROM employees;
SELECT SUM(salary) FROM employees;
SELECT AVG(salary) FROM employees;
SELECT MIN(salary) FROM employees;
SELECT MAX(salary) FROM employees;

SELECT bool_and(active) FROM employees;
SELECT bool_or(active) FROM employees;

SELECT string_agg(name, ',' ORDER BY name)
FROM employees;

SELECT array_agg(id ORDER BY id)
FROM employees;

-- ============================================================
-- 037. FILTER AGGREGATES
-- ============================================================

SELECT
    COUNT(*) FILTER (WHERE active),
    COUNT(*) FILTER (WHERE NOT active)
FROM employees;

SELECT
    SUM(salary) FILTER (WHERE department = 'Engineering')
FROM employees;

-- ============================================================
-- 038. ORDERED AGGREGATES
-- ============================================================

SELECT array_agg(name ORDER BY name)
FROM employees;

SELECT string_agg(name, ',' ORDER BY salary DESC)
FROM employees;

-- ============================================================
-- 039. ARRAYS
-- ============================================================

SELECT ARRAY[1,2,3];
SELECT ARRAY['a','b','c'];
SELECT ARRAY[true,false,NULL];

SELECT ARRAY[1,2,3][1];
SELECT ARRAY[1,2,3][2];
SELECT ARRAY[1,2,3][3];

SELECT ARRAY[1,2,3][4];

SELECT array_length(ARRAY[1,2,3], 1);
SELECT cardinality(ARRAY[1,2,3]);

SELECT array_append(ARRAY[1,2], 3);
SELECT array_prepend(1, ARRAY[2,3]);
SELECT array_cat(ARRAY[1,2], ARRAY[3,4]);

SELECT 2 = ANY(ARRAY[1,2,3]);
SELECT 4 = ANY(ARRAY[1,2,3]);

SELECT 2 = ALL(ARRAY[2,2]);
SELECT 2 = ALL(ARRAY[2,3]);

SELECT unnest(ARRAY[1,2,3]);

-- ============================================================
-- 040. MULTIDIMENSIONAL ARRAYS
-- ============================================================

SELECT ARRAY[
    ARRAY[1,2],
    ARRAY[3,4]
];

SELECT array_ndims(
    ARRAY[
        ARRAY[1,2],
        ARRAY[3,4]
    ]
);

SELECT cardinality(
    ARRAY[
        ARRAY[1,2],
        ARRAY[3,4]
    ]
);

-- ============================================================
-- 041. JSON / JSONB
-- ============================================================

SELECT '{}'::json;
SELECT '{}'::jsonb;
SELECT '[]'::json;
SELECT '[]'::jsonb;
SELECT '"hello"'::json;
SELECT '"hello"'::jsonb;
SELECT '123'::json;
SELECT '123'::jsonb;
SELECT 'true'::json;
SELECT 'null'::jsonb;

SELECT json_typeof('{}'::json);
SELECT json_typeof('[]'::json);
SELECT json_typeof('"x"'::json);
SELECT json_typeof('123'::json);
SELECT json_typeof('true'::json);
SELECT json_typeof('null'::json);

SELECT jsonb_typeof('{}'::jsonb);
SELECT jsonb_typeof('[]'::jsonb);
SELECT jsonb_typeof('"x"'::jsonb);
SELECT jsonb_typeof('123'::jsonb);
SELECT jsonb_typeof('true'::jsonb);
SELECT jsonb_typeof('null'::jsonb);

-- ============================================================
-- 042. JSON BUILDERS
-- ============================================================

SELECT json_build_object(
    'name','PLOMID',
    'version',1,
    'active',true
);

SELECT jsonb_build_object(
    'name','PLOMID',
    'version',1,
    'active',true
);

SELECT json_build_array(1,2,3,NULL);
SELECT jsonb_build_array(1,2,3,NULL);

SELECT json_object(
    ARRAY['a','1','b','2']
);

SELECT jsonb_object(
    ARRAY['a','1','b','2']
);

SELECT json_build_object(
    'nested',
    json_build_object('x',1),
    'array',
    json_build_array(1,2,3)
);

-- ============================================================
-- 043. JSON EXTRACTION
-- ============================================================

SELECT '{"name":"PLOMID"}'::json -> 'name';
SELECT '{"name":"PLOMID"}'::jsonb -> 'name';

SELECT '{"name":"PLOMID"}'::json ->> 'name';
SELECT '{"name":"PLOMID"}'::jsonb ->> 'name';

SELECT '[1,2,3]'::jsonb -> 0;
SELECT '[1,2,3]'::jsonb -> -1;

SELECT
    '{"a":{"b":{"c":123}}}'::jsonb
    #> ARRAY['a','b','c'];

SELECT
    '{"a":{"b":{"c":123}}}'::jsonb
    #>> ARRAY['a','b','c'];

SELECT
    '{"a":{"b":{"c":123}}}'::jsonb
    #> ARRAY['a','missing'];

-- ============================================================
-- 044. JSON ARRAY / OBJECT FUNCTIONS
-- ============================================================

SELECT json_array_length('[1,2,3]'::json);
SELECT jsonb_array_length('[1,2,3]'::jsonb);

SELECT json_array_elements('[1,2,3]'::json);
SELECT jsonb_array_elements('[1,2,3]'::jsonb);

SELECT json_array_elements_text('["a","b"]'::json);
SELECT jsonb_array_elements_text('["a","b"]'::jsonb);

SELECT json_each('{"a":1,"b":2}'::json);
SELECT jsonb_each('{"a":1,"b":2}'::jsonb);

SELECT json_each_text('{"a":1,"b":2}'::json);
SELECT jsonb_each_text('{"a":1,"b":2}'::jsonb);

SELECT json_object_keys('{"a":1,"b":2}'::json);
SELECT jsonb_object_keys('{"a":1,"b":2}'::jsonb);

-- ============================================================
-- 045. JSONB OPERATORS
-- ============================================================

SELECT '{"a":1,"b":2}'::jsonb @> '{"a":1}'::jsonb;
SELECT '{"a":1}'::jsonb <@ '{"a":1,"b":2}'::jsonb;

SELECT '{"a":1}'::jsonb ? 'a';
SELECT '{"a":1}'::jsonb ? 'x';

SELECT '{"a":1}'::jsonb ?| ARRAY['a','x'];
SELECT '{"a":1}'::jsonb ?& ARRAY['a'];

SELECT '{"a":1}'::jsonb || '{"b":2}'::jsonb;
SELECT '{"a":1}'::jsonb || '{"a":2}'::jsonb;

SELECT '{"a":1,"b":2}'::jsonb - 'a';
SELECT '{"a":1,"b":2}'::jsonb - ARRAY['a','b'];

SELECT '{"a":{"b":1}}'::jsonb #- '{a,b}';

-- ============================================================
-- 046. JSONB MUTATION
-- ============================================================

SELECT jsonb_set(
    '{}'::jsonb,
    '{a}',
    '1'::jsonb
);

SELECT jsonb_set(
    '{"a":1}'::jsonb,
    '{a}',
    '2'::jsonb
);

SELECT jsonb_set(
    '{"a":{"b":1}}'::jsonb,
    '{a,b}',
    '99'::jsonb
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,1}',
    '99'::jsonb
);

SELECT jsonb_set_lax(
    '{"a":1}'::jsonb,
    '{a}',
    NULL,
    true,
    'delete_key'
);

SELECT jsonb_insert(
    '["a","b","c"]'::jsonb,
    '{1}',
    '"X"'::jsonb
);

-- ============================================================
-- 047. JSONB STRIP NULLS
-- ============================================================

SELECT json_strip_nulls(
    '{"a":1,"b":null}'::json
);

SELECT jsonb_strip_nulls(
    '{"a":1,"b":null}'::jsonb
);

-- ============================================================
-- 048. JSON AGGREGATION
-- ============================================================

SELECT json_agg(x)
FROM (VALUES (1),(2),(3)) v(x);

SELECT jsonb_agg(x)
FROM (VALUES (1),(2),(3)) v(x);

SELECT json_object_agg(k,v)
FROM (
    VALUES
        ('a','1'),
        ('b','2')
) x(k,v);

SELECT jsonb_object_agg(k,v)
FROM (
    VALUES
        ('a','1'),
        ('b','2')
) x(k,v);

SELECT jsonb_agg(x ORDER BY x DESC)
FROM (VALUES (1),(2),(3)) v(x);

-- ============================================================
-- 049. JSON RECORD CONVERSION
-- ============================================================

CREATE TYPE json_person AS (
    id integer,
    name text,
    active boolean
);

SELECT *
FROM json_to_record(
    '{"id":1,"name":"Alice","active":true}'::json
) AS x(
    id integer,
    name text,
    active boolean
);

SELECT *
FROM jsonb_to_record(
    '{"id":1,"name":"Alice","active":true}'::jsonb
) AS x(
    id integer,
    name text,
    active boolean
);

SELECT *
FROM json_populate_record(
    NULL::json_person,
    '{"id":1,"name":"Alice","active":true}'::json
);

SELECT *
FROM jsonb_populate_record(
    NULL::json_person,
    '{"id":1,"name":"Alice","active":true}'::jsonb
);

-- ============================================================
-- 050. JSON RECORDSET
-- ============================================================

SELECT *
FROM json_to_recordset(
    '[
        {"id":1,"name":"A","active":true},
        {"id":2,"name":"B","active":false}
    ]'::json
) AS x(
    id integer,
    name text,
    active boolean
);

SELECT *
FROM jsonb_to_recordset(
    '[
        {"id":1,"name":"A","active":true},
        {"id":2,"name":"B","active":false}
    ]'::jsonb
) AS x(
    id integer,
    name text,
    active boolean
);

-- ============================================================
-- 051. JSONPATH
-- ============================================================

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.a'
);

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.a ? (@ > 5)'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a > 5'
);

SELECT jsonb_path_query(
    '{"a":10,"b":20}'::jsonb,
    '$.*'
);

SELECT jsonb_path_query_array(
    '{"a":10,"b":20}'::jsonb,
    '$.*'
);

SELECT jsonb_path_query_first(
    '{"a":10,"b":20}'::jsonb,
    '$.*'
);

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.a > $min',
    '{"min":5}'::jsonb
);

SELECT jsonb_path_query(
    '[1,2,3,4,5]'::jsonb,
    '$[*] ? (@ > 2)'
);

SELECT jsonb_path_query(
    '{"items":[1,2,3,4]}'::jsonb,
    '$.items[*] ? (@ >= 2 && @ <= 3)'
);

-- ============================================================
-- 052. SQL/JSON
-- ============================================================

SELECT JSON('{"a":1}');
SELECT JSON('[1,2,3]');

SELECT JSON_SCALAR(123);
SELECT JSON_SCALAR('hello');
SELECT JSON_SCALAR(true);

SELECT JSON_OBJECT(
    KEY 'name' VALUE 'PLOMID',
    KEY 'version' VALUE 1
);

SELECT JSON_ARRAY(1,2,3);

SELECT JSON_VALUE(
    '{"name":"PLOMID"}',
    '$.name'
);

SELECT JSON_VALUE(
    '{"version":123}',
    '$.version'
);

SELECT JSON_QUERY(
    '{"address":{"city":"Hyderabad"}}',
    '$.address'
);

SELECT JSON_EXISTS(
    '{"active":true}',
    '$.active'
);

SELECT JSON_EXISTS(
    '{"active":true}',
    '$.missing'
);

-- ============================================================
-- 053. JSON_TABLE
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"name":"Alice","active":true},
        {"id":2,"name":"Bob","active":false}
    ]',
    '$[*]'
    COLUMNS (
        id integer PATH '$.id',
        name text PATH '$.name',
        active boolean PATH '$.active'
    )
) AS jt;

SELECT *
FROM JSON_TABLE(
    '{
        "customers":[
            {
                "id":1,
                "name":"Alice",
                "orders":[
                    {"id":101,"amount":100},
                    {"id":102,"amount":200}
                ]
            }
        ]
    }',
    '$.customers[*]'
    COLUMNS (
        customer_id integer PATH '$.id',
        name text PATH '$.name',
        NESTED PATH '$.orders[*]'
        COLUMNS (
            order_id integer PATH '$.id',
            amount numeric PATH '$.amount'
        )
    )
) AS jt;

-- ============================================================
-- 054. UUID
-- ============================================================

CREATE TABLE uuid_test (
    id uuid PRIMARY KEY,
    name text
);

INSERT INTO uuid_test VALUES (
    '550e8400-e29b-41d4-a716-446655440000',
    'PLOMID'
);

SELECT * FROM uuid_test;

SELECT gen_random_uuid();

-- ============================================================
-- 055. NETWORK TYPES
-- ============================================================

SELECT '127.0.0.1'::inet;
SELECT '192.168.1.0/24'::cidr;
SELECT '00:11:22:33:44:55'::macaddr;

SELECT host('192.168.1.0/24'::cidr);
SELECT masklen('192.168.1.0/24'::cidr);
SELECT family('127.0.0.1'::inet);

SELECT '192.168.1.10'::inet << '192.168.1.0/24'::inet;

-- ============================================================
-- 056. ENUM
-- ============================================================

CREATE TYPE employee_status AS ENUM (
    'new',
    'active',
    'inactive'
);

CREATE TABLE enum_test (
    id integer,
    status employee_status
);

INSERT INTO enum_test VALUES
(1, 'new'),
(2, 'active'),
(3, 'inactive');

SELECT * FROM enum_test ORDER BY id;

SELECT enum_first(NULL::employee_status);
SELECT enum_last(NULL::employee_status);
SELECT enum_range(NULL::employee_status);

-- ============================================================
-- 057. DOMAIN
-- ============================================================

CREATE DOMAIN positive_integer AS integer
CHECK (VALUE > 0);

CREATE TABLE domain_test (
    id positive_integer
);

INSERT INTO domain_test VALUES (1);
SELECT * FROM domain_test;

INSERT INTO domain_test VALUES (-1);

-- ============================================================
-- 058. COMPOSITE TYPES
-- ============================================================

CREATE TYPE address_type AS (
    city text,
    country text,
    zipcode text
);

CREATE TABLE composite_test (
    id integer,
    name text,
    address address_type
);

INSERT INTO composite_test VALUES (
    1,
    'PLOMID',
    ROW('Hyderabad','India','500001')
);

SELECT * FROM composite_test;

SELECT (address).city
FROM composite_test;

-- ============================================================
-- 059. RANGE TYPES
-- ============================================================

SELECT int4range(1, 10);
SELECT int8range(1, 10);
SELECT numrange(1.0, 10.0);
SELECT tsrange(
    TIMESTAMP '2026-01-01',
    TIMESTAMP '2026-02-01'
);

SELECT '[1,10)'::int4range;
SELECT '[1,10]'::int4range;

SELECT 5 <@ int4range(1,10);
SELECT int4range(1,10) @> 5;

SELECT int4range(1,5) && int4range(4,10);
SELECT int4range(1,5) && int4range(5,10);

-- ============================================================
-- 060. MULTIRANGE
-- ============================================================

SELECT '{[1,3),[5,7)}'::int4multirange;

SELECT 2 <@ '{[1,3),[5,7)}'::int4multirange;
SELECT 4 <@ '{[1,3),[5,7)}'::int4multirange;

SELECT range_merge(
    '{[1,3),[5,7)}'::int4multirange
);

-- ============================================================
-- 061. XML
-- ============================================================

SELECT XML '<root><a>1</a></root>';

SELECT xmlparse(
    document '<root><a>1</a></root>'
);

SELECT xpath(
    '/root/a/text()',
    '<root><a>1</a></root>'
);

SELECT xmlexists(
    '/root/a'
    PASSING BY REF
    '<root><a>1</a></root>'
);

-- ============================================================
-- 062. SEQUENCES
-- ============================================================

CREATE SEQUENCE test_sequence
START 1
INCREMENT 1;

SELECT nextval('test_sequence');
SELECT nextval('test_sequence');
SELECT currval('test_sequence');
SELECT lastval();

SELECT setval('test_sequence', 100);
SELECT nextval('test_sequence');

-- ============================================================
-- 063. IDENTITY
-- ============================================================

CREATE TABLE identity_test (
    id bigint GENERATED ALWAYS AS IDENTITY,
    name text
);

INSERT INTO identity_test(name)
VALUES ('A'), ('B'), ('C');

SELECT * FROM identity_test ORDER BY id;

INSERT INTO identity_test
OVERRIDING SYSTEM VALUE
(id,name)
VALUES
(100,'manual');

SELECT * FROM identity_test ORDER BY id;

-- ============================================================
-- 064. GENERATED COLUMNS
-- ============================================================

CREATE TABLE generated_test (
    id integer,
    first_name text,
    last_name text,
    full_name text GENERATED ALWAYS AS
        (first_name || ' ' || last_name) STORED
);

INSERT INTO generated_test
(id,first_name,last_name)
VALUES
(1,'Alice','Smith'),
(2,'Bob','Jones');

SELECT * FROM generated_test ORDER BY id;

-- ============================================================
-- 065. CONSTRAINTS
-- ============================================================

CREATE TABLE constraint_test (
    id integer PRIMARY KEY,
    name text NOT NULL,
    email text UNIQUE,
    age integer CHECK (age >= 0)
);

INSERT INTO constraint_test VALUES
(1,'Alice','alice@example.com',30);

SELECT * FROM constraint_test;

INSERT INTO constraint_test
VALUES
(1,'Duplicate','duplicate@example.com',20);

INSERT INTO constraint_test
VALUES
(2,NULL,'x@example.com',20);

INSERT INTO constraint_test
VALUES
(3,'Bad','bad@example.com',-1);

-- ============================================================
-- 066. FOREIGN KEYS
-- ============================================================

CREATE TABLE parent_test (
    id integer PRIMARY KEY
);

CREATE TABLE child_test (
    id integer PRIMARY KEY,
    parent_id integer REFERENCES parent_test(id)
);

INSERT INTO parent_test VALUES (1),(2);

INSERT INTO child_test VALUES
(1,1),
(2,2);

SELECT * FROM child_test ORDER BY id;

INSERT INTO child_test VALUES
(3,999);

-- ============================================================
-- 067. CASCADE
-- ============================================================

CREATE TABLE parent_cascade (
    id integer PRIMARY KEY
);

CREATE TABLE child_cascade (
    id integer PRIMARY KEY,
    parent_id integer REFERENCES
        parent_cascade(id)
        ON DELETE CASCADE
);

INSERT INTO parent_cascade VALUES (1);
INSERT INTO child_cascade VALUES (1,1);

DELETE FROM parent_cascade WHERE id = 1;

SELECT * FROM child_cascade;

-- ============================================================
-- 068. DEFERRABLE CONSTRAINT
-- ============================================================

CREATE TABLE def_parent (
    id integer PRIMARY KEY
);

CREATE TABLE def_child (
    id integer PRIMARY KEY,
    parent_id integer,
    CONSTRAINT fk_def
        FOREIGN KEY (parent_id)
        REFERENCES def_parent(id)
        DEFERRABLE INITIALLY DEFERRED
);

BEGIN;

INSERT INTO def_child VALUES (1,100);
INSERT INTO def_parent VALUES (100);

COMMIT;

SELECT * FROM def_child;

-- ============================================================
-- 069. INDEXES
-- ============================================================

CREATE INDEX idx_employee_name
ON employees(name);

CREATE INDEX idx_employee_department
ON employees(department);

CREATE INDEX idx_employee_salary
ON employees(salary);

CREATE UNIQUE INDEX idx_department_name
ON departments(name);

SELECT *
FROM employees
WHERE name = 'Alice';

-- ============================================================
-- 070. EXPRESSION INDEX
-- ============================================================

CREATE INDEX idx_employee_lower_name
ON employees(lower(name));

SELECT *
FROM employees
WHERE lower(name) = 'alice';

-- ============================================================
-- 071. PARTIAL INDEX
-- ============================================================

CREATE INDEX idx_active_employee
ON employees(id)
WHERE active;

SELECT *
FROM employees
WHERE active
ORDER BY id;

-- ============================================================
-- 072. INCLUDE INDEX
-- ============================================================

CREATE INDEX idx_employee_department_include
ON employees(department)
INCLUDE (name, salary);

SELECT *
FROM employees
WHERE department = 'Engineering';

-- ============================================================
-- 073. GIN
-- ============================================================

CREATE TABLE gin_test (
    id integer PRIMARY KEY,
    tags text[]
);

INSERT INTO gin_test VALUES
(1, ARRAY['sql','postgresql']),
(2, ARRAY['json','database']),
(3, ARRAY['sql','json']);

CREATE INDEX idx_gin_tags
ON gin_test
USING GIN(tags);

SELECT *
FROM gin_test
WHERE tags @> ARRAY['sql'];

-- ============================================================
-- 074. BRIN
-- ============================================================

CREATE TABLE brin_test (
    id bigint,
    created_at timestamp
);

INSERT INTO brin_test
SELECT
    g,
    TIMESTAMP '2026-01-01' + g * INTERVAL '1 minute'
FROM generate_series(1,1000) g;

CREATE INDEX idx_brin_created
ON brin_test
USING BRIN(created_at);

SELECT COUNT(*)
FROM brin_test
WHERE created_at >= TIMESTAMP '2026-01-01';

-- ============================================================
-- 075. HASH INDEX
-- ============================================================

CREATE INDEX idx_employee_hash
ON employees
USING HASH(id);

SELECT *
FROM employees
WHERE id = 1;

-- ============================================================
-- 076. GIST RANGE INDEX
-- ============================================================

CREATE TABLE schedule_test (
    id integer,
    period tstzrange
);

INSERT INTO schedule_test VALUES
(
    1,
    tstzrange(
        '2026-01-01 10:00+00',
        '2026-01-01 12:00+00'
    )
),
(
    2,
    tstzrange(
        '2026-01-01 13:00+00',
        '2026-01-01 15:00+00'
    )
);

CREATE INDEX idx_schedule_period
ON schedule_test
USING GIST(period);

SELECT *
FROM schedule_test
WHERE period @>
    TIMESTAMPTZ '2026-01-01 11:00+00';

-- ============================================================
-- 077. VIEWS
-- ============================================================

CREATE VIEW employee_view AS
SELECT
    id,
    name,
    department,
    salary
FROM employees;

SELECT * FROM employee_view ORDER BY id;

-- ============================================================
-- 078. MATERIALIZED VIEW
-- ============================================================

CREATE MATERIALIZED VIEW employee_summary AS
SELECT
    department,
    COUNT(*) AS employee_count,
    AVG(salary) AS avg_salary
FROM employees
GROUP BY department;

SELECT *
FROM employee_summary
ORDER BY department;

REFRESH MATERIALIZED VIEW employee_summary;

-- ============================================================
-- 079. FUNCTIONS
-- ============================================================

CREATE FUNCTION add_numbers(a integer, b integer)
RETURNS integer
LANGUAGE SQL
IMMUTABLE
AS $$
    SELECT a + b;
$$;

SELECT add_numbers(10,20);

CREATE FUNCTION employee_count()
RETURNS bigint
LANGUAGE SQL
STABLE
AS $$
    SELECT COUNT(*)
    FROM employees;
$$;

SELECT employee_count();

-- ============================================================
-- 080. PLPGSQL
-- ============================================================

CREATE FUNCTION multiply_numbers(a integer, b integer)
RETURNS integer
LANGUAGE plpgsql
AS $$
BEGIN
    RETURN a * b;
END;
$$;

SELECT multiply_numbers(6,7);

CREATE FUNCTION employee_label(p_id integer)
RETURNS text
LANGUAGE plpgsql
AS $$
DECLARE
    result text;
BEGIN
    SELECT name || ':' || department
    INTO result
    FROM employees
    WHERE id = p_id;

    RETURN result;
END;
$$;

SELECT employee_label(1);

-- ============================================================
-- 081. PLPGSQL CONTROL FLOW
-- ============================================================

CREATE FUNCTION classify_number(n integer)
RETURNS text
LANGUAGE plpgsql
AS $$
BEGIN
    IF n < 0 THEN
        RETURN 'negative';
    ELSIF n = 0 THEN
        RETURN 'zero';
    ELSE
        RETURN 'positive';
    END IF;
END;
$$;

SELECT classify_number(-1);
SELECT classify_number(0);
SELECT classify_number(1);

-- ============================================================
-- 082. PLPGSQL LOOP
-- ============================================================

CREATE FUNCTION sum_numbers(n integer)
RETURNS integer
LANGUAGE plpgsql
AS $$
DECLARE
    i integer;
    total integer := 0;
BEGIN
    FOR i IN 1..n LOOP
        total := total + i;
    END LOOP;

    RETURN total;
END;
$$;

SELECT sum_numbers(10);

-- ============================================================
-- 083. PROCEDURE
-- ============================================================

CREATE TABLE procedure_test (
    id integer,
    value text
);

CREATE PROCEDURE insert_procedure_row(
    p_id integer,
    p_value text
)
LANGUAGE SQL
AS $$
    INSERT INTO procedure_test
    VALUES (p_id, p_value);
$$;

CALL insert_procedure_row(1,'hello');

SELECT * FROM procedure_test;

-- ============================================================
-- 084. TRIGGERS
-- ============================================================

CREATE TABLE audit_test (
    id integer,
    value text,
    updated_at timestamp
);

CREATE FUNCTION set_updated_at()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.updated_at := CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_set_updated_at
BEFORE INSERT OR UPDATE
ON audit_test
FOR EACH ROW
EXECUTE FUNCTION set_updated_at();

INSERT INTO audit_test(id,value)
VALUES (1,'hello');

SELECT * FROM audit_test;

UPDATE audit_test
SET value = 'world'
WHERE id = 1;

SELECT * FROM audit_test;

-- ============================================================
-- 085. TRANSACTIONS
-- ============================================================

BEGIN;

INSERT INTO procedure_test
VALUES (2,'transaction');

SELECT *
FROM procedure_test
ORDER BY id;

COMMIT;

-- ============================================================
-- 086. ROLLBACK
-- ============================================================

BEGIN;

INSERT INTO procedure_test
VALUES (3,'rollback');

ROLLBACK;

SELECT *
FROM procedure_test
ORDER BY id;

-- ============================================================
-- 087. SAVEPOINT
-- ============================================================

BEGIN;

INSERT INTO procedure_test
VALUES (4,'before savepoint');

SAVEPOINT test_savepoint;

INSERT INTO procedure_test
VALUES (5,'after savepoint');

ROLLBACK TO SAVEPOINT test_savepoint;

COMMIT;

SELECT *
FROM procedure_test
ORDER BY id;

-- ============================================================
-- 088. PREPARED STATEMENTS
-- ============================================================

PREPARE employee_by_id(integer) AS
SELECT *
FROM employees
WHERE id = $1;

EXECUTE employee_by_id(1);
EXECUTE employee_by_id(2);

DEALLOCATE employee_by_id;

-- ============================================================
-- 089. CURSOR
-- ============================================================

BEGIN;

DECLARE employee_cursor CURSOR FOR
SELECT id, name
FROM employees
ORDER BY id;

FETCH NEXT FROM employee_cursor;
FETCH NEXT FROM employee_cursor;
FETCH ALL FROM employee_cursor;

CLOSE employee_cursor;

COMMIT;

-- ============================================================
-- 090. TEMPORARY TABLE
-- ============================================================

CREATE TEMP TABLE temp_test (
    id integer,
    value text
);

INSERT INTO temp_test VALUES
(1,'a'),
(2,'b');

SELECT * FROM temp_test ORDER BY id;

-- ============================================================
-- 091. UNLOGGED TABLE
-- ============================================================

CREATE UNLOGGED TABLE unlogged_test (
    id integer,
    value text
);

INSERT INTO unlogged_test VALUES
(1,'hello');

SELECT * FROM unlogged_test;

-- ============================================================
-- 092. PARTITIONING
-- ============================================================

CREATE TABLE partition_test (
    id integer,
    category integer,
    value text
)
PARTITION BY RANGE(category);

CREATE TABLE partition_test_a
PARTITION OF partition_test
FOR VALUES FROM (0) TO (10);

CREATE TABLE partition_test_b
PARTITION OF partition_test
FOR VALUES FROM (10) TO (20);

INSERT INTO partition_test VALUES
(1,5,'A'),
(2,15,'B');

SELECT tableoid::regclass, *
FROM partition_test
ORDER BY id;

-- ============================================================
-- 093. LATERAL
-- ============================================================

SELECT
    e.id,
    e.name,
    x.n
FROM employees e
CROSS JOIN LATERAL (
    SELECT e.id * 10 AS n
) x
ORDER BY e.id;

-- ============================================================
-- 094. ROW VALUES / COMPARISON
-- ============================================================

SELECT ROW(1,2) = ROW(1,2);
SELECT ROW(1,2) <> ROW(1,3);

SELECT ROW(1,2) < ROW(2,1);

SELECT (1,2) IS DISTINCT FROM (1,2);
SELECT (1,2) IS DISTINCT FROM (1,3);

-- ============================================================
-- 095. SYSTEM FUNCTIONS
-- ============================================================

SELECT current_database();
SELECT current_schema();
SELECT current_user;
SELECT session_user;
SELECT current_role;

SELECT current_timestamp;
SELECT current_date;
SELECT current_time;

SELECT pg_backend_pid();
SELECT pg_postmaster_start_time();

-- ============================================================
-- 096. INFORMATION SCHEMA
-- ============================================================

SELECT table_schema, table_name
FROM information_schema.tables
WHERE table_schema = 'pg17_compat'
ORDER BY table_name;

SELECT column_name, data_type
FROM information_schema.columns
WHERE table_schema = 'pg17_compat'
  AND table_name = 'employees'
ORDER BY ordinal_position;

SELECT constraint_name, constraint_type
FROM information_schema.table_constraints
WHERE table_schema = 'pg17_compat'
ORDER BY constraint_name;

-- ============================================================
-- 097. SYSTEM CATALOGS
-- ============================================================

SELECT
    n.nspname,
    c.relname,
    c.relkind
FROM pg_class c
JOIN pg_namespace n
    ON n.oid = c.relnamespace
WHERE n.nspname = 'pg17_compat'
ORDER BY c.relname;

SELECT
    p.proname
FROM pg_proc p
JOIN pg_namespace n
    ON n.oid = p.pronamespace
WHERE n.nspname = 'pg17_compat'
ORDER BY p.proname;

-- ============================================================
-- 098. EXPLAIN
-- ============================================================

EXPLAIN
SELECT *
FROM employees
WHERE department = 'Engineering';

EXPLAIN (COSTS OFF)
SELECT
    department,
    COUNT(*)
FROM employees
GROUP BY department;

EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)
SELECT *
FROM employees
WHERE id = 1;

-- ============================================================
-- 099. LARGE DATA / GENERATE_SERIES
-- ============================================================

SELECT COUNT(*)
FROM generate_series(1,100000);

SELECT SUM(i)
FROM generate_series(1,100000) AS g(i);

SELECT COUNT(*)
FROM generate_series(
    DATE '2026-01-01',
    DATE '2026-01-31',
    INTERVAL '1 day'
) AS g(d);

CREATE TABLE large_test (
    id bigint PRIMARY KEY,
    value numeric
);

INSERT INTO large_test
SELECT
    i,
    i * 1.2345
FROM generate_series(1,10000) AS g(i);

SELECT COUNT(*) FROM large_test;
SELECT SUM(value) FROM large_test;
SELECT AVG(value) FROM large_test;

-- ============================================================
-- 100. JSON LARGE DOCUMENT
-- ============================================================

SELECT jsonb_array_length(
    jsonb_agg(i)
)
FROM generate_series(1,10000) AS g(i);

SELECT COUNT(*)
FROM jsonb_array_elements(
    (
        SELECT jsonb_agg(i)
        FROM generate_series(1,10000) AS g(i)
    )
);

-- ============================================================
-- 101. JSON ENTERPRISE WORKLOAD
-- ============================================================

CREATE TABLE json_data (
    id integer PRIMARY KEY,
    payload jsonb NOT NULL
);

INSERT INTO json_data VALUES
(
    1,
    '{
        "id":1,
        "name":"PLOMID",
        "version":1,
        "active":true,
        "score":99.95,
        "tags":["sql","json","postgresql"],
        "address":{
            "city":"Hyderabad",
            "country":"India",
            "zipcode":"500001"
        },
        "customer":{
            "id":1001,
            "tier":"enterprise"
        }
    }'
),
(
    2,
    '{
        "id":2,
        "name":"TEST",
        "version":2,
        "active":false,
        "score":88.50,
        "tags":["database"],
        "address":{
            "city":"Bangalore",
            "country":"India"
        },
        "customer":{
            "id":1002,
            "tier":"standard"
        }
    }'
),
(
    3,
    '{
        "id":3,
        "name":"DEMO",
        "version":3,
        "active":true,
        "score":91.10,
        "tags":["sql","database"]
    }'
);

CREATE INDEX idx_json_data_payload
ON json_data
USING GIN(payload);

CREATE INDEX idx_json_data_name
ON json_data((payload ->> 'name'));

SELECT *
FROM json_data
WHERE payload @> '{"active":true}'::jsonb
ORDER BY id;

SELECT *
FROM json_data
WHERE payload ? 'customer'
ORDER BY id;

SELECT
    id,
    payload ->> 'name',
    payload #>> '{address,city}'
FROM json_data
ORDER BY id;

SELECT jsonb_agg(
    jsonb_build_object(
        'id', id,
        'name', payload ->> 'name',
        'active', payload -> 'active'
    )
)
FROM json_data;

EXPLAIN
SELECT *
FROM json_data
WHERE payload @> '{"customer":{"tier":"enterprise"}}'::jsonb;

-- ============================================================
-- 102. FULL TEXT SEARCH
-- ============================================================

CREATE TABLE fts_test (
    id integer PRIMARY KEY,
    body text
);

INSERT INTO fts_test VALUES
(1,'PostgreSQL database JSON testing'),
(2,'SQL compatibility testing'),
(3,'Enterprise database platform');

SELECT
    id,
    to_tsvector('english', body)
FROM fts_test
ORDER BY id;

SELECT
    id,
    to_tsvector('english', body)
        @@ plainto_tsquery('english','database')
FROM fts_test
ORDER BY id;

SELECT
    ts_headline(
        'english',
        body,
        plainto_tsquery('english','database')
    )
FROM fts_test
WHERE id = 1;

-- ============================================================
-- 103. ERROR / EDGE CASES
-- ============================================================

SELECT 'not-a-number'::integer;

SELECT 'not-a-date'::date;

SELECT '{"broken":}'::json;

SELECT '{"broken":}'::jsonb;

SELECT '[1,2'::json;

SELECT 1 / 0;

SELECT sqrt(-1);

SELECT 1::integer / 0::integer;

SELECT CAST('abc' AS integer);

-- Missing JSON paths should normally return NULL.
SELECT '{}'::jsonb -> 'missing';
SELECT '{}'::jsonb ->> 'missing';

SELECT '[1,2,3]'::jsonb -> 99;
SELECT '[1,2,3]'::jsonb -> -99;

-- ============================================================
-- 104. FINAL CROSS-FEATURE TEST
-- ============================================================

SELECT
    d.id,
    d.payload ->> 'name' AS name,
    d.payload ->> 'active' AS active,
    d.payload #>> '{address,city}' AS city,
    COUNT(e.id) AS employees
FROM json_data d
LEFT JOIN employees e
    ON e.department =
       CASE
           WHEN d.id = 1 THEN 'Engineering'
           WHEN d.id = 2 THEN 'Finance'
           ELSE 'HR'
       END
GROUP BY
    d.id,
    d.payload
ORDER BY d.id;

SELECT
    jsonb_agg(
        jsonb_build_object(
            'employee_id', e.id,
            'employee', e.name,
            'department', e.department,
            'salary', e.salary,
            'json_name', d.payload ->> 'name'
        )
        ORDER BY e.id
    )
FROM employees e
CROSS JOIN LATERAL (
    SELECT payload
    FROM json_data
    WHERE id = 1
) d;

-- ============================================================
-- FINAL OBJECT / TYPE CHECKS
-- ============================================================

SELECT
    current_setting('server_version') AS server_version,
    current_database() AS database_name,
    current_schema() AS schema_name,
    current_user AS current_user_name;

SELECT
    'PostgreSQL 17 compatibility workload completed'
        AS status;

-- ============================================================
-- CLEANUP
-- ============================================================

RESET search_path;
RESET TIME ZONE;

DROP SCHEMA pg17_compat CASCADE;

SELECT 'PLOMID / PostgreSQL compatibility test suite finished'
    AS status;
