-- ============================================================
-- PLOMID
-- POSTGRESQL 17 JSON / JSONB ENTERPRISE COMPATIBILITY SUITE
--
-- TARGET:
--   PostgreSQL 17
--
-- PURPOSE:
--   Exhaustive PostgreSQL-native JSON / JSONB compatibility
--   and regression suite.
--
-- IMPORTANT:
--   This suite intentionally uses PostgreSQL syntax/features.
--   It is NOT intended to be portable SQL.
--
-- ERROR POLICY:
--   ON_ERROR_STOP is disabled so negative/error tests can run.
--
-- COVERAGE:
--
--   001  Types / casts
--   002  JSON validity
--   003  Type inspection
--   004  Constructors
--   005  Arrays
--   006  Objects
--   007  Nested construction
--   008  SQL type conversion
--   009  Composite conversion
--   010  Extraction
--   011  Path extraction
--   012  Array expansion
--   013  Object expansion
--   014  Containment
--   015  Existence
--   016  Concatenation
--   017  Deletion
--   018  jsonb_set
--   019  jsonb_set_lax
--   020  jsonb_insert
--   021  Strip nulls
--   022  Pretty printing
--   023  Aggregates
--   024  Aggregate ordering/filtering
--   025  Duplicate keys
--   026  NULL / missing semantics
--   027  Numeric precision
--   028  Dates/timestamps
--   029  UUID / network / misc types
--   030  Arrays of SQL types
--   031  Multidimensional arrays
--   032  Record conversion
--   033  Recordset conversion
--   034  Populate record
--   035  Populate recordset
--   036  JSON_TABLE
--   037  SQL/JSON constructors
--   038  JSON_VALUE
--   039  JSON_QUERY
--   040  JSON_EXISTS
--   041  JSONPath
--   042  JSONPath variables
--   043  JSONPath predicates
--   044  JSONPath arrays
--   045  JSONPath strict/lax
--   046  JSONPath timezone variants
--   047  JSONB equality/order
--   048  DISTINCT/GROUP BY/hash behavior
--   049  Indexes
--   050  GIN default
--   051  GIN jsonb_path_ops
--   052  Expression indexes
--   053  Generated columns
--   054  Constraints
--   055  Defaults
--   056  Views
--   057  Joins
--   058  LATERAL workloads
--   059  Large documents
--   060  Large arrays
--   061  Deep documents
--   062  Unicode
--   063  Escaping
--   064  Whitespace/canonicalization
--   065  Empty structures
--   066  Boolean/null edge cases
--   067  Negative indexes
--   068  Mutation edge cases
--   069  Invalid-input/error tests
--   070  SQL/JSON error behavior
--   071  JSON_TABLE error behavior
--   072  Object aggregate edge cases
--   073  Aggregate empty-set behavior
--   074  API response workload
--   075  Enterprise document workload
--   076  JSONB statistics/planning
--   077  Cast round trips
--   078  Domain behavior
--   079  Composite nested JSON
--   080  Final regression checks
-- ============================================================

\set ON_ERROR_STOP off

SET client_min_messages = NOTICE;

DROP SCHEMA IF EXISTS json_enterprise CASCADE;
DROP SCHEMA IF EXISTS json_all_proof CASCADE;

CREATE SCHEMA json_enterprise;
CREATE SCHEMA json_all_proof;


-- ============================================================
-- 001. BASE TYPES / CASTS
-- ============================================================

SELECT '{}'::json;
SELECT '{}'::jsonb;

SELECT '[]'::json;
SELECT '[]'::jsonb;

SELECT '"hello"'::json;
SELECT '"hello"'::jsonb;

SELECT '123'::json;
SELECT '123'::jsonb;

SELECT '-123'::json;
SELECT '-123'::jsonb;

SELECT '123.456'::json;
SELECT '123.456'::jsonb;

SELECT '1e10'::json;
SELECT '1e10'::jsonb;

SELECT 'true'::json;
SELECT 'true'::jsonb;

SELECT 'false'::json;
SELECT 'false'::jsonb;

SELECT 'null'::json;
SELECT 'null'::jsonb;

SELECT '{}'::json::jsonb;
SELECT '{}'::jsonb::json;

SELECT '[]'::json::jsonb;
SELECT '[]'::jsonb::json;

SELECT '"hello"'::json::jsonb;
SELECT '"hello"'::jsonb::json;


-- ============================================================
-- 002. JSON VALIDATION
-- ============================================================

SELECT '{}' IS JSON;
SELECT '[]' IS JSON;
SELECT '"hello"' IS JSON;
SELECT '123' IS JSON;
SELECT 'true' IS JSON;
SELECT 'false' IS JSON;
SELECT 'null' IS JSON;

SELECT '{}' IS JSON OBJECT;
SELECT '[]' IS JSON ARRAY;
SELECT '"hello"' IS JSON SCALAR;
SELECT '123' IS JSON SCALAR;
SELECT 'true' IS JSON SCALAR;
SELECT 'null' IS JSON SCALAR;

SELECT '{}' IS JSON VALUE;
SELECT '[]' IS JSON VALUE;
SELECT '"x"' IS JSON VALUE;
SELECT '123' IS JSON VALUE;
SELECT 'true' IS JSON VALUE;
SELECT 'null' IS JSON VALUE;

SELECT '{"a":1}' IS JSON OBJECT;
SELECT '[1,2,3]' IS JSON ARRAY;


-- ============================================================
-- 003. TYPE INSPECTION
-- ============================================================

SELECT json_typeof('{}'::json);
SELECT json_typeof('[]'::json);
SELECT json_typeof('"x"'::json);
SELECT json_typeof('123'::json);
SELECT json_typeof('true'::json);
SELECT json_typeof('false'::json);
SELECT json_typeof('null'::json);

SELECT jsonb_typeof('{}'::jsonb);
SELECT jsonb_typeof('[]'::jsonb);
SELECT jsonb_typeof('"x"'::jsonb);
SELECT jsonb_typeof('123'::jsonb);
SELECT jsonb_typeof('true'::jsonb);
SELECT jsonb_typeof('false'::jsonb);
SELECT jsonb_typeof('null'::jsonb);

SELECT json_typeof(NULL::json);
SELECT jsonb_typeof(NULL::jsonb);


-- ============================================================
-- FIXTURES
-- ============================================================

CREATE TABLE json_enterprise.data (
    id INTEGER PRIMARY KEY,
    payload JSONB NOT NULL
);

INSERT INTO json_enterprise.data(id,payload)
VALUES
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
    }'::jsonb
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
    }'::jsonb
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
    }'::jsonb
);

CREATE TABLE json_enterprise.customers (
    id INTEGER PRIMARY KEY,
    data JSONB NOT NULL
);

INSERT INTO json_enterprise.customers
VALUES
(1001,'{"name":"Alice","tier":"enterprise"}'),
(1002,'{"name":"Bob","tier":"standard"}'),
(1003,'{"name":"Carol","tier":"enterprise"}');


-- ============================================================
-- COMPOSITE TYPES
-- ============================================================

CREATE TYPE json_enterprise.person AS (
    id INTEGER,
    name TEXT,
    active BOOLEAN
);

CREATE TYPE json_all_proof.person AS (
    name TEXT,
    version INTEGER,
    active BOOLEAN
);


-- ============================================================
-- 004. JSON OBJECT CONSTRUCTORS
-- ============================================================

SELECT json_build_object();

SELECT json_build_object(
    'name','PLOMID',
    'version',1,
    'active',true
);

SELECT json_build_object(
    'name','PLOMID',
    'missing',NULL
);

SELECT jsonb_build_object();

SELECT jsonb_build_object(
    'name','PLOMID',
    'version',1,
    'active',true
);

SELECT jsonb_build_object(
    'name','PLOMID',
    'missing',NULL
);

SELECT json_build_object(
    'nested',
    json_build_object(
        'a',1,
        'b',2
    )
);

SELECT jsonb_build_object(
    'nested',
    jsonb_build_object(
        'a',1,
        'b',2
    )
);


-- ============================================================
-- 005. JSON ARRAY CONSTRUCTORS
-- ============================================================

SELECT json_build_array();

SELECT json_build_array(
    1,2,3
);

SELECT json_build_array(
    'PLOMID',
    1,
    true,
    false,
    NULL
);

SELECT jsonb_build_array();

SELECT jsonb_build_array(
    1,2,3
);

SELECT jsonb_build_array(
    'PLOMID',
    1,
    true,
    false,
    NULL
);

SELECT json_build_array(
    json_build_object('a',1),
    json_build_array(1,2,3)
);

SELECT jsonb_build_array(
    jsonb_build_object('a',1),
    jsonb_build_array(1,2,3)
);


-- ============================================================
-- 006. JSON OBJECT ARRAY FORMS
-- ============================================================

SELECT json_object(
    ARRAY['a','1','b','2']
);

SELECT jsonb_object(
    ARRAY['a','1','b','2']
);

SELECT json_object(
    ARRAY['a','b'],
    ARRAY['1','2']
);

SELECT jsonb_object(
    ARRAY['a','b'],
    ARRAY['1','2']
);

SELECT json_object(
    ARRAY[]::text[]
);

SELECT jsonb_object(
    ARRAY[]::text[]
);


-- ============================================================
-- 007. NESTED CONSTRUCTION
-- ============================================================

SELECT jsonb_build_object(
    'id',1,
    'name','PLOMID',
    'customer',
    jsonb_build_object(
        'id',1001,
        'tier','enterprise'
    ),
    'tags',
    jsonb_build_array(
        'sql',
        'json',
        'postgresql'
    )
);


-- ============================================================
-- 008. SQL TYPE -> JSON
-- ============================================================

SELECT to_json(NULL::integer);
SELECT to_json(0);
SELECT to_json(123);
SELECT to_json(-123);
SELECT to_json(123.45::numeric);
SELECT to_json(true);
SELECT to_json(false);
SELECT to_json('PLOMID'::text);

SELECT to_jsonb(NULL::integer);
SELECT to_jsonb(0);
SELECT to_jsonb(123);
SELECT to_jsonb(-123);
SELECT to_jsonb(123.45::numeric);
SELECT to_jsonb(true);
SELECT to_jsonb(false);
SELECT to_jsonb('PLOMID'::text);

SELECT to_json(ARRAY[1,2,3]);
SELECT to_jsonb(ARRAY[1,2,3]);

SELECT to_json(ARRAY['a','b','c']);
SELECT to_jsonb(ARRAY['a','b','c']);


-- ============================================================
-- 009. ROW / COMPOSITE
-- ============================================================

SELECT row_to_json(
    ROW(1,'PLOMID',true)
);

SELECT row_to_json(x)
FROM (
    SELECT
        1 AS id,
        'PLOMID' AS name,
        true AS active
) x;

SELECT to_json(
    ROW(1,'PLOMID',true)
);

SELECT to_jsonb(
    ROW(1,'PLOMID',true)
);

SELECT to_json(
    ROW(
        'PLOMID',
        1,
        true
    )::json_all_proof.person
);

SELECT to_jsonb(
    ROW(
        'PLOMID',
        1,
        true
    )::json_all_proof.person
);


-- ============================================================
-- 010. BASIC EXTRACTION
-- ============================================================

SELECT '{"name":"PLOMID"}'::json -> 'name';
SELECT '{"name":"PLOMID"}'::jsonb -> 'name';

SELECT '{"name":"PLOMID"}'::json ->> 'name';
SELECT '{"name":"PLOMID"}'::jsonb ->> 'name';

SELECT '[10,20,30]'::json -> 0;
SELECT '[10,20,30]'::json -> 1;
SELECT '[10,20,30]'::json -> 2;

SELECT '[10,20,30]'::jsonb -> 0;
SELECT '[10,20,30]'::jsonb -> 1;
SELECT '[10,20,30]'::jsonb -> 2;

SELECT '[10,20,30]'::jsonb ->> 0;
SELECT '[10,20,30]'::jsonb ->> 1;
SELECT '[10,20,30]'::jsonb ->> 2;


-- ============================================================
-- 011. PATH EXTRACTION
-- ============================================================

SELECT
    '{"a":{"b":{"c":123}}}'::json
    #> ARRAY['a','b','c'];

SELECT
    '{"a":{"b":{"c":123}}}'::jsonb
    #> ARRAY['a','b','c'];

SELECT
    '{"a":{"b":{"c":123}}}'::json
    #>> ARRAY['a','b','c'];

SELECT
    '{"a":{"b":{"c":123}}}'::jsonb
    #>> ARRAY['a','b','c'];

SELECT json_extract_path(
    '{"a":{"b":{"c":123}}}'::json,
    'a','b','c'
);

SELECT jsonb_extract_path(
    '{"a":{"b":{"c":123}}}'::jsonb,
    'a','b','c'
);

SELECT json_extract_path_text(
    '{"a":{"b":{"c":"hello"}}}'::json,
    'a','b','c'
);

SELECT jsonb_extract_path_text(
    '{"a":{"b":{"c":"hello"}}}'::jsonb,
    'a','b','c'
);


-- ============================================================
-- 012. ARRAY ELEMENT FUNCTIONS
-- ============================================================

SELECT json_array_length('[1,2,3]'::json);
SELECT json_array_length('[]'::json);

SELECT jsonb_array_length('[1,2,3]'::jsonb);
SELECT jsonb_array_length('[]'::jsonb);

SELECT json_array_elements('[1,2,3]'::json);
SELECT jsonb_array_elements('[1,2,3]'::jsonb);

SELECT json_array_elements_text('["a","b","c"]'::json);
SELECT jsonb_array_elements_text('["a","b","c"]'::jsonb);

SELECT jsonb_array_elements(
    '[1,true,null,"hello",{"x":1},[1,2]]'::jsonb
);

SELECT jsonb_array_elements_text(
    '["a","b",null,true,123]'::jsonb
);


-- ============================================================
-- 013. OBJECT ELEMENT FUNCTIONS
-- ============================================================

SELECT json_each(
    '{"a":1,"b":2,"c":3}'::json
);

SELECT jsonb_each(
    '{"a":1,"b":2,"c":3}'::jsonb
);

SELECT json_each_text(
    '{"a":1,"b":2,"c":3}'::json
);

SELECT jsonb_each_text(
    '{"a":1,"b":2,"c":3}'::jsonb
);

SELECT json_object_keys(
    '{"a":1,"b":2,"c":3}'::json
);

SELECT jsonb_object_keys(
    '{"a":1,"b":2,"c":3}'::jsonb
);


-- ============================================================
-- 014. CONTAINMENT
-- ============================================================

SELECT '{"a":1,"b":2}'::jsonb @> '{"a":1}'::jsonb;
SELECT '{"a":1}'::jsonb <@ '{"a":1,"b":2}'::jsonb;

SELECT '["a","b","c"]'::jsonb @> '["a","b"]'::jsonb;
SELECT '["a","b"]'::jsonb <@ '["a","b","c"]'::jsonb;

SELECT '{}'::jsonb @> '{}'::jsonb;
SELECT '[]'::jsonb @> '[]'::jsonb;

SELECT '{"a":null}'::jsonb @> '{"a":null}'::jsonb;
SELECT '{"a":null}'::jsonb @> '{}'::jsonb;

SELECT
    '{"a":[1,2,3]}'::jsonb
    @>
    '{"a":[2]}'::jsonb;

SELECT
    '{"address":{"country":"India"}}'::jsonb
    @>
    '{"address":{"country":"India"}}'::jsonb;


-- ============================================================
-- 015. EXISTENCE
-- ============================================================

SELECT '{"a":1,"b":2}'::jsonb ? 'a';
SELECT '{"a":1,"b":2}'::jsonb ? 'missing';

SELECT '{"a":1,"b":2}'::jsonb ?| ARRAY['x','a'];
SELECT '{"a":1,"b":2}'::jsonb ?| ARRAY['x','y'];

SELECT '{"a":1,"b":2}'::jsonb ?& ARRAY['a','b'];
SELECT '{"a":1,"b":2}'::jsonb ?& ARRAY['a','x'];

SELECT '{}'::jsonb ?| ARRAY[]::text[];
SELECT '{}'::jsonb ?& ARRAY[]::text[];

SELECT '{"a":null}'::jsonb ? 'a';


-- ============================================================
-- 016. CONCATENATION
-- ============================================================

SELECT '{}'::jsonb || '{}'::jsonb;
SELECT '{}'::jsonb || '{"a":1}'::jsonb;
SELECT '{"a":1}'::jsonb || '{}'::jsonb;

SELECT '{"a":1,"b":2}'::jsonb ||
       '{"c":3}'::jsonb;

SELECT '{"a":1,"b":2}'::jsonb ||
       '{"b":99,"c":3}'::jsonb;

SELECT '[]'::jsonb || '[]'::jsonb;
SELECT '[]'::jsonb || '[1,2]'::jsonb;
SELECT '[1,2]'::jsonb || '[]'::jsonb;

SELECT '[1,2]'::jsonb || '[3,4]'::jsonb;

SELECT '{"a":1}'::jsonb || '[2,3]'::jsonb;
SELECT '[1,2]'::jsonb || '{"a":3}'::jsonb;


-- ============================================================
-- 017. DELETE OPERATORS
-- ============================================================

SELECT '{"a":1,"b":2}'::jsonb - 'a';
SELECT '{"a":1,"b":2}'::jsonb - 'missing';

SELECT '{"a":1,"b":2,"c":3}'::jsonb
       - ARRAY['a','b'];

SELECT '["a","b","c"]'::jsonb - 0;
SELECT '["a","b","c"]'::jsonb - 1;
SELECT '["a","b","c"]'::jsonb - 2;

SELECT '[1,2,3]'::jsonb - -1;
SELECT '[1,2,3]'::jsonb - -2;

SELECT
    '{"a":{"b":1,"c":2}}'::jsonb
    #- ARRAY['a','b'];

SELECT
    '{"a":{"b":1}}'::jsonb
    #- ARRAY['missing','b'];


-- ============================================================
-- 018. JSONB_SET
-- ============================================================

SELECT jsonb_set(
    '{}'::jsonb,
    '{a}',
    '1'::jsonb
);

SELECT jsonb_set(
    '{}'::jsonb,
    '{a}',
    '1'::jsonb,
    false
);

SELECT jsonb_set(
    '{}'::jsonb,
    '{a}',
    '1'::jsonb,
    true
);

SELECT jsonb_set(
    '{"a":1}'::jsonb,
    '{a}',
    '99'::jsonb
);

SELECT jsonb_set(
    '{"a":{"b":1}}'::jsonb,
    '{a,b}',
    '99'::jsonb
);

SELECT jsonb_set(
    '{"a":{}}'::jsonb,
    '{a,b}',
    '1'::jsonb,
    true
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,0}',
    '99'::jsonb
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,1}',
    '99'::jsonb
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,-1}',
    '99'::jsonb
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,10}',
    '99'::jsonb
);


-- ============================================================
-- 019. JSONB_SET_LAX
-- ============================================================

SELECT jsonb_set_lax(
    '{"a":1}'::jsonb,
    '{a}',
    NULL
);

SELECT jsonb_set_lax(
    '{"a":1}'::jsonb,
    '{a}',
    NULL,
    true,
    'delete_key'
);

SELECT jsonb_set_lax(
    '{"a":1}'::jsonb,
    '{a}',
    NULL,
    true,
    'use_json_null'
);

SELECT jsonb_set_lax(
    '{"a":1}'::jsonb,
    '{a}',
    NULL,
    true,
    'return_target'
);

SELECT jsonb_set_lax(
    '{"a":1}'::jsonb,
    '{a}',
    NULL,
    true,
    'raise_exception'
);


-- ============================================================
-- 020. JSONB_INSERT
-- ============================================================

SELECT jsonb_insert(
    '{"a":1,"b":2}'::jsonb,
    '{c}',
    '99'::jsonb
);

SELECT jsonb_insert(
    '{"a":1,"b":2}'::jsonb,
    '{b}',
    '99'::jsonb
);

SELECT jsonb_insert(
    '[]'::jsonb,
    '{0}',
    '"a"'::jsonb
);

SELECT jsonb_insert(
    '["a","b"]'::jsonb,
    '{0}',
    '"x"'::jsonb
);

SELECT jsonb_insert(
    '["a","b"]'::jsonb,
    '{1}',
    '"x"'::jsonb
);

SELECT jsonb_insert(
    '["a","b"]'::jsonb,
    '{1}',
    '"x"'::jsonb,
    true
);

SELECT jsonb_insert(
    '["a","b"]'::jsonb,
    '{1}',
    '"x"'::jsonb,
    false
);

SELECT jsonb_insert(
    '["a","b","c"]'::jsonb,
    '{-1}',
    '"x"'::jsonb
);

SELECT jsonb_insert(
    '["a","b","c"]'::jsonb,
    '{10}',
    '"x"'::jsonb
);


-- ============================================================
-- 021. STRIP NULLS
-- ============================================================

SELECT json_strip_nulls(
    '{"a":1,"b":null,"c":2}'::json
);

SELECT jsonb_strip_nulls(
    '{"a":1,"b":null,"c":2}'::jsonb
);

SELECT json_strip_nulls(
    '{"a":null,"nested":{"x":1,"y":null}}'::json
);

SELECT jsonb_strip_nulls(
    '{"a":null,"nested":{"x":1,"y":null}}'::jsonb
);

SELECT jsonb_strip_nulls('{}'::jsonb);
SELECT jsonb_strip_nulls('[]'::jsonb);

SELECT jsonb_strip_nulls(
    '[null,1,null,2]'::jsonb
);

SELECT jsonb_strip_nulls(
    '{"a":{"b":null,"c":1}}'::jsonb
);


-- ============================================================
-- 022. PRETTY
-- ============================================================

SELECT jsonb_pretty(
    '{
        "name":"PLOMID",
        "version":1,
        "active":true
    }'::jsonb
);


-- ============================================================
-- 023. AGGREGATES
-- ============================================================

SELECT json_agg(1);
SELECT jsonb_agg(1);

SELECT json_agg(NULL);
SELECT jsonb_agg(NULL);

SELECT json_agg(value)
FROM (
    VALUES (1),(2),(3)
) x(value);

SELECT jsonb_agg(value)
FROM (
    VALUES (1),(2),(3)
) x(value);

SELECT json_agg(value)
FROM (
    SELECT 1 AS value
    WHERE FALSE
) x;

SELECT jsonb_agg(value)
FROM (
    SELECT 1 AS value
    WHERE FALSE
) x;


-- ============================================================
-- 024. AGGREGATE ORDERING / FILTER
-- ============================================================

SELECT json_agg(value ORDER BY value)
FROM (
    VALUES (3),(1),(2)
) x(value);

SELECT jsonb_agg(value ORDER BY value DESC)
FROM (
    VALUES (3),(1),(2)
) x(value);

SELECT jsonb_agg(value)
FILTER (WHERE value > 1)
FROM (
    VALUES (1),(2),(3)
) x(value);

SELECT json_object_agg(key,value)
FROM (
    VALUES
        ('a','1'),
        ('b','2')
) x(key,value);

SELECT jsonb_object_agg(key,value)
FROM (
    VALUES
        ('b','2'),
        ('a','1')
) x(key,value);

SELECT jsonb_object_agg(key,value ORDER BY key)
FROM (
    VALUES
        ('c','3'),
        ('a','1'),
        ('b','2')
) x(key,value);

SELECT jsonb_object_agg(key,value)
FILTER (WHERE value IS NOT NULL)
FROM (
    VALUES
        ('a','1'),
        ('b',NULL),
        ('c','3')
) x(key,value);


-- ============================================================
-- 025. DUPLICATE KEYS
-- ============================================================

SELECT '{"a":1,"a":2}'::json;
SELECT '{"a":1,"a":2}'::jsonb;

SELECT '{"a":1,"a":2,"a":3}'::json;
SELECT '{"a":1,"a":2,"a":3}'::jsonb;

SELECT '{"a":1,"a":2,"a":3}'::jsonb ->> 'a';

SELECT json_build_object(
    'a',1,
    'a',2
);

SELECT jsonb_build_object(
    'a',1,
    'a',2
);


-- ============================================================
-- 026. NULL VS MISSING
-- ============================================================

SELECT '{"a":null}'::jsonb -> 'a';
SELECT '{"a":null}'::jsonb ->> 'a';

SELECT '{}'::jsonb -> 'a';
SELECT '{}'::jsonb ->> 'a';

SELECT '{"a":null}'::jsonb #> '{a}';
SELECT '{}'::jsonb #> '{a}';

SELECT '{"a":null}'::jsonb ? 'a';
SELECT '{}'::jsonb ? 'a';

SELECT jsonb_build_object(
    'a',
    NULL
);


-- ============================================================
-- 027. NUMERIC PRECISION
-- ============================================================

SELECT to_json(12345678901234567890::numeric);
SELECT to_jsonb(12345678901234567890::numeric);

SELECT
    '{"amount":12345678901234567890}'::jsonb;

SELECT
    '{"amount":12345678901234567890.123456789}'::jsonb;

SELECT jsonb_build_object(
    'amount',
    999999999999999999.999999999::numeric
);

SELECT '9223372036854775807'::jsonb;
SELECT '-9223372036854775808'::jsonb;

SELECT
    '999999999999999999999999999999999999999999999999'::jsonb;

SELECT
    '0.000000000000000000000000000001'::jsonb;

SELECT '1e100'::jsonb;
SELECT '1e-100'::jsonb;


-- ============================================================
-- 028. DATE / TIME
-- ============================================================

SELECT to_json(DATE '2026-01-01');
SELECT to_jsonb(DATE '2026-01-01');

SELECT to_json(TIME '12:30:45');
SELECT to_jsonb(TIME '12:30:45');

SELECT to_json(TIMESTAMP '2026-01-01 12:30:45');
SELECT to_jsonb(TIMESTAMP '2026-01-01 12:30:45');

SELECT to_json(TIMESTAMPTZ '2026-01-01 12:30:45+05:30');
SELECT to_jsonb(TIMESTAMPTZ '2026-01-01 12:30:45+05:30');

SELECT to_json(INTERVAL '1 day 2 hours');
SELECT to_jsonb(INTERVAL '1 day 2 hours');


-- ============================================================
-- 029. SPECIAL SQL TYPES
-- ============================================================

SELECT to_json(
    '550e8400-e29b-41d4-a716-446655440000'::uuid
);

SELECT to_jsonb(
    '550e8400-e29b-41d4-a716-446655440000'::uuid
);

SELECT to_json('127.0.0.1'::inet);
SELECT to_jsonb('127.0.0.1'::inet);

SELECT to_json('127.0.0.1/24'::cidr);
SELECT to_jsonb('127.0.0.1/24'::cidr);

SELECT to_json('550e8400-e29b-41d4-a716-446655440000'::uuid);


-- ============================================================
-- 030. ARRAYS OF SQL TYPES
-- ============================================================

SELECT to_json(ARRAY[1,2,3]::integer[]);
SELECT to_jsonb(ARRAY[1,2,3]::integer[]);

SELECT to_json(ARRAY['a','b','c']::text[]);
SELECT to_jsonb(ARRAY['a','b','c']::text[]);

SELECT to_json(ARRAY[true,false,NULL]::boolean[]);
SELECT to_jsonb(ARRAY[true,false,NULL]::boolean[]);

SELECT to_json(ARRAY[1.1,2.2,3.3]::numeric[]);


-- ============================================================
-- 031. MULTIDIMENSIONAL ARRAYS
-- ============================================================

SELECT to_json(
    ARRAY[
        ARRAY[1,2],
        ARRAY[3,4]
    ]
);

SELECT to_jsonb(
    ARRAY[
        ARRAY[1,2],
        ARRAY[3,4]
    ]
);


-- ============================================================
-- 032. JSON TO RECORD
-- ============================================================

SELECT *
FROM json_to_record(
    '{"id":1,"name":"PLOMID","active":true}'::json
) AS x(
    id INTEGER,
    name TEXT,
    active BOOLEAN
);

SELECT *
FROM jsonb_to_record(
    '{"id":1,"name":"PLOMID","active":true}'::jsonb
) AS x(
    id INTEGER,
    name TEXT,
    active BOOLEAN
);

SELECT *
FROM jsonb_to_record(
    '{"id":"42","name":"PLOMID","active":"true"}'::jsonb
) AS x(
    id INTEGER,
    name TEXT,
    active BOOLEAN
);


-- ============================================================
-- 033. RECORDSET
-- ============================================================

SELECT *
FROM json_to_recordset(
    '[
        {"id":1,"name":"A","active":true},
        {"id":2,"name":"B","active":false}
    ]'::json
) AS x(
    id INTEGER,
    name TEXT,
    active BOOLEAN
);

SELECT *
FROM jsonb_to_recordset(
    '[
        {"id":1,"name":"A","active":true},
        {"id":2,"name":"B","active":false}
    ]'::jsonb
) AS x(
    id INTEGER,
    name TEXT,
    active BOOLEAN
);


-- ============================================================
-- 034. POPULATE RECORD
-- ============================================================

SELECT *
FROM json_populate_record(
    NULL::json_enterprise.person,
    '{"id":1,"name":"PLOMID","active":true}'::json
);

SELECT *
FROM jsonb_populate_record(
    NULL::json_enterprise.person,
    '{"id":1,"name":"PLOMID","active":true}'::jsonb
);


-- ============================================================
-- 035. POPULATE RECORDSET
-- ============================================================

SELECT *
FROM json_populate_recordset(
    NULL::json_enterprise.person,
    '[
        {"id":1,"name":"A","active":true},
        {"id":2,"name":"B","active":false}
    ]'::json
);

SELECT *
FROM jsonb_populate_recordset(
    NULL::json_enterprise.person,
    '[
        {"id":1,"name":"A","active":true},
        {"id":2,"name":"B","active":false}
    ]'::jsonb
);


-- ============================================================
-- 036. JSON_TABLE BASIC
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"name":"Alice","active":true},
        {"id":2,"name":"Bob","active":false}
    ]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        name TEXT PATH '$.name',
        active BOOLEAN PATH '$.active'
    )
) AS jt;


-- ============================================================
-- 036B. JSON_TABLE NESTED
-- ============================================================

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
        customer_id INTEGER PATH '$.id',
        name TEXT PATH '$.name',
        NESTED PATH '$.orders[*]'
        COLUMNS (
            order_id INTEGER PATH '$.id',
            amount NUMERIC PATH '$.amount'
        )
    )
) AS jt;


-- ============================================================
-- 037. SQL/JSON CONSTRUCTORS
-- ============================================================

SELECT JSON('{"a":1}');
SELECT JSON('[1,2,3]');

SELECT JSON_SCALAR(123);
SELECT JSON_SCALAR('hello');
SELECT JSON_SCALAR(true);
SELECT JSON_SCALAR(NULL);

SELECT JSON_OBJECT(
    KEY 'name' VALUE 'PLOMID',
    KEY 'version' VALUE 1
);

SELECT JSON_ARRAY(
    1,
    2,
    3
);

SELECT JSON_ARRAY(
    'a',
    'b',
    'c'
);

SELECT JSON_ARRAY(
    1,
    NULL,
    3
);

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1,
    KEY 'b' VALUE NULL
);


-- ============================================================
-- 038. JSON_VALUE
-- ============================================================

SELECT JSON_VALUE(
    '{"name":"PLOMID"}',
    '$.name'
);

SELECT JSON_VALUE(
    '{"version":123}',
    '$.version'
);

SELECT JSON_VALUE(
    '{"version":123}',
    '$.version'
    RETURNING INTEGER
);

SELECT JSON_VALUE(
    '{"score":99.95}',
    '$.score'
    RETURNING NUMERIC
);

SELECT JSON_VALUE(
    '{"active":true}',
    '$.active'
    RETURNING BOOLEAN
);

SELECT JSON_VALUE(
    '{"x":"hello"}',
    '$.missing'
);


-- ============================================================
-- 039. JSON_QUERY
-- ============================================================

SELECT JSON_QUERY(
    '{"address":{"city":"Hyderabad"}}',
    '$.address'
);

SELECT JSON_QUERY(
    '{"tags":["sql","json"]}',
    '$.tags'
);

SELECT JSON_QUERY(
    '{"x":123}',
    '$.missing'
);

SELECT JSON_QUERY(
    '{"x":{"a":1}}',
    '$.x'
);


-- ============================================================
-- 040. JSON_EXISTS
-- ============================================================

SELECT JSON_EXISTS(
    '{"active":true}',
    '$.active'
);

SELECT JSON_EXISTS(
    '{"active":true}',
    '$.missing'
);

SELECT JSON_EXISTS(
    '{"a":[1,2,3]}',
    '$.a[*] ? (@ > 1)'
);


-- ============================================================
-- 041. JSONPATH BASIC
-- ============================================================

SELECT jsonb_path_exists(
    '{"a":10,"b":20}'::jsonb,
    '$.a'
);

SELECT jsonb_path_exists(
    '{"a":10,"b":20}'::jsonb,
    '$.missing'
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

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a > 5'
);


-- ============================================================
-- 042. JSONPATH VARIABLES
-- ============================================================

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.a > $min',
    '{"min":5}'::jsonb
);

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.a > $min',
    '{"min":20}'::jsonb
);

SELECT jsonb_path_query(
    '{"a":10,"b":20}'::jsonb,
    '$.* ? (@ > $min)',
    '{"min":15}'::jsonb
);


-- ============================================================
-- 043. JSONPATH PREDICATES
-- ============================================================

SELECT jsonb_path_query(
    '{
        "customers":[
            {"id":1,"active":true,"score":90},
            {"id":2,"active":false,"score":70},
            {"id":3,"active":true,"score":95}
        ]
    }'::jsonb,
    '$.customers[*] ? (@.active == true)'
);

SELECT jsonb_path_query(
    '{
        "customers":[
            {"id":1,"score":90},
            {"id":2,"score":70},
            {"id":3,"score":95}
        ]
    }'::jsonb,
    '$.customers[*] ? (@.score >= 90)'
);

SELECT jsonb_path_query(
    '{"items":[1,2,3,4]}'::jsonb,
    '$.items[*] ? (@ >= 2 && @ <= 3)'
);


-- ============================================================
-- 044. JSONPATH ARRAYS
-- ============================================================

SELECT jsonb_path_query(
    '{"tags":["sql","json","database"]}'::jsonb,
    '$.tags[*]'
);

SELECT jsonb_path_query_array(
    '{"tags":["sql","json","database"]}'::jsonb,
    '$.tags[*]'
);

SELECT jsonb_path_query(
    '[1,2,3,4,5]'::jsonb,
    '$[*] ? (@ > 2)'
);

SELECT jsonb_path_query(
    '["a","b","c"]'::jsonb,
    '$[*] ? (@ == "b")'
);

SELECT jsonb_path_query(
    '[1,2,3,4,5]'::jsonb,
    '$.size()'
);


-- ============================================================
-- 045. JSONPATH STRICT / LAX
-- ============================================================

SELECT jsonb_path_query(
    '{"a":1}'::jsonb,
    'lax $.a'
);

SELECT jsonb_path_query(
    '{"a":1}'::jsonb,
    'strict $.a'
);


-- ============================================================
-- 046. JSONPATH TIMEZONE VARIANTS
-- ============================================================

SELECT jsonb_path_exists_tz(
    '{"date":"2026-01-01T12:00:00+05:30"}'::jsonb,
    '$.date'
);

SELECT jsonb_path_match_tz(
    '{"value":10}'::jsonb,
    '$.value > 5'
);

SELECT jsonb_path_query_tz(
    '{"a":10,"b":20}'::jsonb,
    '$.*'
);

SELECT jsonb_path_query_array_tz(
    '{"a":10,"b":20}'::jsonb,
    '$.*'
);

SELECT jsonb_path_query_first_tz(
    '{"a":10,"b":20}'::jsonb,
    '$.*'
);


-- ============================================================
-- 047. JSONB EQUALITY / ORDERING
-- ============================================================

SELECT '{"a":1}'::jsonb = '{"a":1}'::jsonb;
SELECT '{"a":1}'::jsonb <> '{"a":2}'::jsonb;

SELECT '{"a":1}'::jsonb < '{"a":2}'::jsonb;
SELECT '{"a":2}'::jsonb > '{"a":1}'::jsonb;

SELECT '{"a":1}'::jsonb <= '{"a":1}'::jsonb;
SELECT '{"a":1}'::jsonb >= '{"a":1}'::jsonb;

SELECT '{"b":2,"a":1}'::jsonb =
       '{"a":1,"b":2}'::jsonb;

SELECT '1'::jsonb < '2'::jsonb;
SELECT '1'::jsonb < '"a"'::jsonb;
SELECT '[]'::jsonb < '{}'::jsonb;


-- ============================================================
-- 048. DISTINCT / GROUP BY
-- ============================================================

SELECT
    payload,
    COUNT(*)
FROM (
    VALUES
        ('{"a":1}'::jsonb),
        ('{"a":1}'::jsonb),
        ('{"a":2}'::jsonb)
) x(payload)
GROUP BY payload;

SELECT DISTINCT payload
FROM (
    VALUES
        ('{"a":1}'::jsonb),
        ('{"a":1}'::jsonb),
        ('{"a":2}'::jsonb)
) x(payload);

SELECT
    value,
    COUNT(*)
FROM (
    VALUES
        ('null'::jsonb),
        ('true'::jsonb),
        ('1'::jsonb),
        ('"x"'::jsonb),
        ('[]'::jsonb),
        ('{}'::jsonb)
) x(value)
GROUP BY value;


-- ============================================================
-- 049. BASIC INDEX
-- ============================================================

CREATE INDEX idx_json_enterprise_payload
ON json_enterprise.data
USING GIN(payload);


-- ============================================================
-- 050. GIN DEFAULT
-- ============================================================

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload @> '{"active":true}'::jsonb;

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload ? 'name';

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload ?| ARRAY['name','missing'];

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload ?& ARRAY['name','active'];


-- ============================================================
-- 051. GIN JSONB PATH OPS
-- ============================================================

CREATE INDEX idx_json_path_ops
ON json_enterprise.data
USING GIN(payload jsonb_path_ops);

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload @> '{"customer":{"tier":"enterprise"}}'::jsonb;


-- ============================================================
-- 052. EXPRESSION INDEX
-- ============================================================

CREATE INDEX idx_json_enterprise_name
ON json_enterprise.data(
    (payload ->> 'name')
);

SELECT *
FROM json_enterprise.data
WHERE payload ->> 'name' = 'PLOMID';

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload ->> 'name' = 'PLOMID';


-- ============================================================
-- 053. GENERATED COLUMN
-- ============================================================

DROP TABLE IF EXISTS json_enterprise.generated_test CASCADE;

CREATE TABLE json_enterprise.generated_test (
    id INTEGER PRIMARY KEY,
    payload JSONB NOT NULL,
    name TEXT
        GENERATED ALWAYS AS
        (payload ->> 'name') STORED
);

INSERT INTO json_enterprise.generated_test(id,payload)
VALUES
(1,'{"name":"PLOMID","active":true}'),
(2,'{"name":"TEST","active":false}');

SELECT *
FROM json_enterprise.generated_test
ORDER BY id;

CREATE INDEX idx_generated_json_name
ON json_enterprise.generated_test(name);

EXPLAIN
SELECT *
FROM json_enterprise.generated_test
WHERE name = 'PLOMID';


-- ============================================================
-- 054. CONSTRAINTS
-- ============================================================

DROP TABLE IF EXISTS json_enterprise.constraint_test CASCADE;

CREATE TABLE json_enterprise.constraint_test (
    id INTEGER PRIMARY KEY,
    payload JSONB,

    CHECK (
        jsonb_typeof(payload) = 'object'
    ),

    CHECK (
        payload ? 'name'
    )
);

INSERT INTO json_enterprise.constraint_test
VALUES (
    1,
    '{"name":"PLOMID"}'
);

SELECT *
FROM json_enterprise.constraint_test;


-- ============================================================
-- 055. DEFAULTS
-- ============================================================

DROP TABLE IF EXISTS json_enterprise.defaults_test CASCADE;

CREATE TABLE json_enterprise.defaults_test (
    id INTEGER PRIMARY KEY,
    payload JSONB DEFAULT '{}'::jsonb
);

INSERT INTO json_enterprise.defaults_test(id)
VALUES (1);

INSERT INTO json_enterprise.defaults_test(id,payload)
VALUES
(2,'{"name":"PLOMID"}');

SELECT *
FROM json_enterprise.defaults_test
ORDER BY id;


-- ============================================================
-- 056. VIEWS
-- ============================================================

DROP VIEW IF EXISTS json_enterprise.data_view;

CREATE VIEW json_enterprise.data_view AS
SELECT
    id,
    payload ->> 'name' AS name,
    payload ->> 'active' AS active,
    payload #>> '{address,city}' AS city
FROM json_enterprise.data;

SELECT *
FROM json_enterprise.data_view
ORDER BY id;


-- ============================================================
-- 057. JSONB JOIN
-- ============================================================

SELECT
    d.id,
    c.data ->> 'name' AS customer_name
FROM json_enterprise.data d
JOIN json_enterprise.customers c
    ON (d.payload -> 'customer' ->> 'id')::INTEGER = c.id
ORDER BY d.id;


-- ============================================================
-- 058. LATERAL JSON WORKLOAD
-- ============================================================

SELECT
    d.id,
    tag
FROM json_enterprise.data d
CROSS JOIN LATERAL
    jsonb_array_elements_text(
        d.payload -> 'tags'
    ) AS tag
ORDER BY d.id, tag;

SELECT
    d.id,
    element
FROM json_enterprise.data d
CROSS JOIN LATERAL
    jsonb_array_elements(
        d.payload -> 'tags'
    ) AS element
ORDER BY d.id;


-- ============================================================
-- 059. LARGE DOCUMENT
-- ============================================================

SELECT jsonb_build_object(
    'company',
    jsonb_build_object(
        'name','PLOMID',
        'departments',
        jsonb_build_array(
            jsonb_build_object(
                'name','Engineering',
                'employees',
                jsonb_build_array(
                    jsonb_build_object(
                        'id',1,
                        'name','Alice'
                    ),
                    jsonb_build_object(
                        'id',2,
                        'name','Bob'
                    )
                )
            ),
            jsonb_build_object(
                'name','Finance',
                'employees',
                jsonb_build_array(
                    jsonb_build_object(
                        'id',3,
                        'name','Carol'
                    )
                )
            )
        )
    )
);


-- ============================================================
-- 060. LARGE JSON ARRAY
-- ============================================================

SELECT jsonb_array_length(
    (
        SELECT jsonb_agg(i)
        FROM generate_series(1,10000) g(i)
    )
);

SELECT count(*)
FROM jsonb_array_elements(
    (
        SELECT jsonb_agg(i)
        FROM generate_series(1,10000) g(i)
    )
);


-- ============================================================
-- 061. DEEP DOCUMENT
-- ============================================================

SELECT jsonb_build_object(
    'a',
    jsonb_build_object(
        'b',
        jsonb_build_object(
            'c',
            jsonb_build_object(
                'd',
                jsonb_build_object(
                    'e',
                    jsonb_build_object(
                        'f',
                        jsonb_build_object(
                            'g',
                            123
                        )
                    )
                )
            )
        )
    )
);

SELECT
    '{"a":{"b":{"c":{"d":{"e":{"f":{"g":123}}}}}}}'::jsonb
    #>> '{a,b,c,d,e,f,g}';


-- ============================================================
-- 062. UNICODE
-- ============================================================

SELECT
    '{"name":"PLOMID Hyderabad"}'::jsonb;

SELECT
    '{"name":"हैदराबाद"}'::jsonb;

SELECT
    '{"name":"東京"}'::jsonb;

SELECT
    '{"name":"Москва"}'::jsonb;

SELECT
    '{"name":"العربية"}'::jsonb;

SELECT
    '{"emoji":"😀"}'::jsonb;

SELECT jsonb_build_object(
    'unicode',
    'हैदराबाद'
);

SELECT jsonb_build_array(
    '😀',
    '🚀',
    '数据库',
    'مرحبا'
);


-- ============================================================
-- 063. ESCAPING
-- ============================================================

SELECT
    '{"text":"hello\\nworld"}'::jsonb;

SELECT
    '{"text":"hello\\tworld"}'::jsonb;

SELECT
    '{"text":"quote: \"hello\""}'::jsonb;

SELECT
    '{"text":"backslash: \\\\"}'::jsonb;

SELECT jsonb_build_object(
    'quote',
    '"hello"'
);

SELECT jsonb_build_object(
    'newline',
    E'hello\nworld'
);

SELECT jsonb_build_object(
    'tab',
    E'hello\tworld'
);


-- ============================================================
-- 064. WHITESPACE / CANONICALIZATION
-- ============================================================

SELECT
    '{"a":1,"b":2}'::jsonb =
    '{
        "a":1,
        "b":2
    }'::jsonb;

SELECT
    '{"b":2,"a":1}'::jsonb =
    '{"a":1,"b":2}'::jsonb;

SELECT '{"b":2,"a":1}'::jsonb;
SELECT ' { "a" : 1 } '::jsonb;


-- ============================================================
-- 065. EMPTY STRUCTURES
-- ============================================================

SELECT '{}'::json;
SELECT '{}'::jsonb;

SELECT '[]'::json;
SELECT '[]'::jsonb;

SELECT json_build_object();
SELECT jsonb_build_object();

SELECT json_build_array();
SELECT jsonb_build_array();

SELECT jsonb_array_length('[]'::jsonb);

SELECT jsonb_agg(value)
FROM (
    SELECT 1 AS value
    WHERE FALSE
) x;

SELECT jsonb_object_agg(key,value)
FROM (
    SELECT 'a' AS key, '1' AS value
    WHERE FALSE
) x;


-- ============================================================
-- 066. BOOLEAN / NULL EDGE CASES
-- ============================================================

SELECT 'true'::jsonb;
SELECT 'false'::jsonb;
SELECT 'null'::jsonb;

SELECT jsonb_typeof('true'::jsonb);
SELECT jsonb_typeof('false'::jsonb);
SELECT jsonb_typeof('null'::jsonb);

SELECT
    'true'::jsonb = 'true'::jsonb;

SELECT
    'true'::jsonb <> 'false'::jsonb;

SELECT
    'null'::jsonb = 'null'::jsonb;

SELECT
    'null'::jsonb IS NULL;

SELECT
    ('{"a":null}'::jsonb -> 'a') IS NULL;

SELECT
    ('{}'::jsonb -> 'a') IS NULL;


-- ============================================================
-- 067. NEGATIVE INDEXES
-- ============================================================

SELECT '[10,20,30]'::jsonb -> -1;
SELECT '[10,20,30]'::jsonb -> -2;
SELECT '[10,20,30]'::jsonb -> -3;
SELECT '[10,20,30]'::jsonb -> -4;

SELECT '[10,20,30]'::jsonb ->> -1;
SELECT '[10,20,30]'::jsonb ->> -4;

SELECT '[10,20,30]'::jsonb #> ARRAY['-1'];
SELECT '[10,20,30]'::jsonb #>> ARRAY['-1'];


-- ============================================================
-- 068. MUTATION EDGE CASES
-- ============================================================

SELECT jsonb_set(
    '{}'::jsonb,
    '{a,b,c}',
    '1'::jsonb,
    false
);

SELECT jsonb_set(
    '{}'::jsonb,
    '{a,b,c}',
    '1'::jsonb,
    true
);

SELECT jsonb_set(
    '{"a":{"b":1}}'::jsonb,
    '{a,b}',
    '2'::jsonb
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,99}',
    '4'::jsonb
);

SELECT jsonb_set(
    '{"a":[1,2,3]}'::jsonb,
    '{a,-99}',
    '4'::jsonb
);

SELECT jsonb_insert(
    '{"a":1}'::jsonb,
    '{missing}',
    '2'::jsonb
);

SELECT jsonb_insert(
    '[]'::jsonb,
    '{999}',
    '1'::jsonb
);


-- ============================================================
-- 069. EXPECTED ERROR TESTS
-- ============================================================

DO $$
BEGIN
    PERFORM '{}'::jsonb - 1;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED ERROR: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM jsonb_array_length('{}'::jsonb);
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED ERROR: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM jsonb_array_elements('{}'::jsonb);
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED ERROR: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM jsonb_each('[]'::jsonb);
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED ERROR: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM jsonb_object_agg(NULL,'x');
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED ERROR: %', SQLERRM;
END
$$;


-- ============================================================
-- 070. SQL/JSON CONVERSION ERRORS
-- ============================================================

DO $$
BEGIN
    PERFORM JSON_VALUE(
        '{"x":"hello"}',
        '$.x'
        RETURNING INTEGER
    );
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED JSON_VALUE CONVERSION ERROR: %',
            SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM JSON_VALUE(
        '{"x":123}',
        '$.x'
        RETURNING DATE
    );
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED JSON_VALUE DATE ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 071. JSON_TABLE ERROR / MISSING BEHAVIOR
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"name":"Alice"},
        {"id":2}
    ]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        name TEXT PATH '$.name'
    )
) AS jt;

DO $$
BEGIN
    PERFORM *
    FROM JSON_TABLE(
        '[{"id":"not-an-integer"}]',
        '$[*]'
        COLUMNS (
            id INTEGER PATH '$.id'
        )
    ) AS jt;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED JSON_TABLE CONVERSION ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 072. OBJECT AGGREGATE EDGE CASES
-- ============================================================

SELECT json_object_agg(
    key,
    value
)
FROM (
    VALUES
        ('a','1'),
        ('b',NULL)
) x(key,value);

SELECT jsonb_object_agg(
    key,
    value
)
FROM (
    VALUES
        ('a','1'),
        ('b',NULL)
) x(key,value);

DO $$
BEGIN
    PERFORM jsonb_object_agg(
        key,
        value
    )
    FROM (
        VALUES
            (NULL::text,'x'::text)
    ) x(key,value);
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED NULL KEY ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 073. EMPTY AGGREGATE SEMANTICS
-- ============================================================

SELECT json_agg(x)
FROM (
    SELECT 1 AS x
    WHERE FALSE
) s;

SELECT jsonb_agg(x)
FROM (
    SELECT 1 AS x
    WHERE FALSE
) s;

SELECT json_object_agg(k,v)
FROM (
    SELECT 'a' AS k, '1' AS v
    WHERE FALSE
) s;

SELECT jsonb_object_agg(k,v)
FROM (
    SELECT 'a' AS k, '1' AS v
    WHERE FALSE
) s;


-- ============================================================
-- 074. API RESPONSE WORKLOAD
-- ============================================================

SELECT jsonb_agg(
    jsonb_build_object(
        'id',id,
        'name',payload ->> 'name',
        'active',payload -> 'active',
        'city',payload #>> '{address,city}',
        'tags',payload -> 'tags'
    )
)
FROM json_enterprise.data;


-- ============================================================
-- 075. ENTERPRISE DOCUMENT WORKLOAD
-- ============================================================

WITH orders AS (
    SELECT
        jsonb_build_object(
            'id',gs,
            'customer',
            jsonb_build_object(
                'id',1000 + gs,
                'name','Customer ' || gs
            ),
            'items',
            jsonb_build_array(
                jsonb_build_object(
                    'sku','SKU-' || gs,
                    'quantity',gs,
                    'price',gs * 10
                )
            ),
            'status',
            CASE
                WHEN gs % 2 = 0
                THEN 'completed'
                ELSE 'pending'
            END
        ) AS document
    FROM generate_series(1,100) gs
)
SELECT count(*)
FROM orders
WHERE document @> '{"status":"completed"}'::jsonb;


-- ============================================================
-- 076. STATISTICS / PLANNING
-- ============================================================

ANALYZE json_enterprise.data;

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload @> '{"active":true}'::jsonb;

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload ->> 'name' = 'PLOMID';

EXPLAIN
SELECT *
FROM json_enterprise.data
WHERE payload ? 'customer';


-- ============================================================
-- 077. ROUND TRIPS
-- ============================================================

SELECT
    value::jsonb::json::jsonb
FROM (
    VALUES
        ('{}'::json),
        ('[]'::json),
        ('{"a":1}'::json),
        ('[1,2,3]'::json),
        ('"hello"'::json),
        ('true'::json),
        ('null'::json)
) x(value);

SELECT
    value::json::jsonb::json
FROM (
    VALUES
        ('{}'::jsonb),
        ('[]'::jsonb),
        ('{"a":1}'::jsonb),
        ('[1,2,3]'::jsonb),
        ('"hello"'::jsonb),
        ('true'::jsonb),
        ('null'::jsonb)
) x(value);


-- ============================================================
-- 078. DOMAIN BEHAVIOR
-- ============================================================

DROP DOMAIN IF EXISTS json_enterprise.json_object_domain CASCADE;

CREATE DOMAIN json_enterprise.json_object_domain AS jsonb
CHECK (
    jsonb_typeof(VALUE) = 'object'
);

CREATE TABLE json_enterprise.domain_test (
    id INTEGER PRIMARY KEY,
    payload json_enterprise.json_object_domain
);

INSERT INTO json_enterprise.domain_test
VALUES
(
    1,
    '{"name":"PLOMID"}'
);

SELECT *
FROM json_enterprise.domain_test;


-- ============================================================
-- 079. NESTED COMPOSITE JSON
-- ============================================================

CREATE TYPE json_enterprise.address AS (
    city TEXT,
    country TEXT
);

CREATE TYPE json_enterprise.customer AS (
    id INTEGER,
    name TEXT,
    address json_enterprise.address
);

SELECT to_json(
    ROW(
        1001,
        'Alice',
        ROW(
            'Hyderabad',
            'India'
        )::json_enterprise.address
    )::json_enterprise.customer
);

SELECT to_jsonb(
    ROW(
        1001,
        'Alice',
        ROW(
            'Hyderabad',
            'India'
        )::json_enterprise.address
    )::json_enterprise.customer
);


-- ============================================================
-- 080. FINAL REGRESSION / INTEGRATION
-- ============================================================

SELECT
    id,
    payload ->> 'name' AS name,
    payload ->> 'active' AS active,
    payload #>> '{address,city}' AS city,
    jsonb_array_length(
        COALESCE(
            payload -> 'tags',
            '[]'::jsonb
        )
    ) AS tag_count
FROM json_enterprise.data
ORDER BY id;

SELECT
    payload ->> 'active' AS active,
    jsonb_agg(
        payload ->> 'name'
        ORDER BY payload ->> 'name'
    ) AS names
FROM json_enterprise.data
GROUP BY payload ->> 'active'
ORDER BY active;

SELECT
    payload ->> 'active' AS active,
    jsonb_object_agg(
        id::text,
        payload
        ORDER BY id
    ) AS documents
FROM json_enterprise.data
GROUP BY payload ->> 'active'
ORDER BY active;

SELECT
    id,
    jsonb_path_query_first(
        payload,
        '$.name'
    ) AS name
FROM json_enterprise.data
ORDER BY id;

SELECT *
FROM json_enterprise.data
ORDER BY id;


-- ============================================================
-- FINAL CLEANUP VERIFICATION
-- ============================================================

SELECT
    n.nspname AS schema_name,
    c.relname AS relation_name,
    c.relkind
FROM pg_class c
JOIN pg_namespace n
    ON n.oid = c.relnamespace
WHERE n.nspname = 'json_enterprise'
ORDER BY c.relname;

SELECT
    n.nspname AS schema_name,
    t.typname AS type_name
FROM pg_type t
JOIN pg_namespace n
    ON n.oid = t.typnamespace
WHERE n.nspname IN (
    'json_enterprise',
    'json_all_proof'
)
ORDER BY n.nspname,t.typname;


CREATE SCHEMA IF NOT EXISTS json_gap_081;


-- ============================================================
-- 081. JSONB SUBSCRIPTING
-- ============================================================

SELECT
    '{"a":1,"b":2}'::jsonb['a'];

SELECT
    '{"a":{"b":123}}'::jsonb['a']['b'];

SELECT
    '{"a":[10,20,30]}'::jsonb['a'][0];

SELECT
    '{"a":[10,20,30]}'::jsonb['a'][2];

SELECT
    '{"a":[10,20,30]}'::jsonb['a'][-1];

SELECT
    '{"a":null}'::jsonb['a'];

SELECT
    '{}'::jsonb['missing'];


-- ============================================================
-- 082. JSONB SUBSCRIPTING MUTATION
-- ============================================================

DROP TABLE IF EXISTS json_gap_081.subscript_test;

CREATE TABLE json_gap_081.subscript_test (
    id INTEGER PRIMARY KEY,
    payload JSONB
);

INSERT INTO json_gap_081.subscript_test
VALUES
    (1, '{"name":"PLOMID","active":true}'),
    (2, '{"items":[1,2,3]}'),
    (3, '{}');

UPDATE json_gap_081.subscript_test
SET payload['name'] = '"UPDATED"'::jsonb
WHERE id = 1;

UPDATE json_gap_081.subscript_test
SET payload['active'] = 'false'::jsonb
WHERE id = 1;

UPDATE json_gap_081.subscript_test
SET payload['new_key'] = '123'::jsonb
WHERE id = 3;

UPDATE json_gap_081.subscript_test
SET payload['items'][1] = '99'::jsonb
WHERE id = 2;

SELECT *
FROM json_gap_081.subscript_test
ORDER BY id;


-- ============================================================
-- 083. SQL/JSON CONSTRUCTOR RETURNING
-- ============================================================

SELECT JSON(
    '{"a":1}'
    RETURNING json
);

SELECT JSON(
    '{"a":1}'
    RETURNING jsonb
);

SELECT JSON_ARRAY(
    1,2,3
    RETURNING json
);

SELECT JSON_ARRAY(
    1,2,3
    RETURNING jsonb
);

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1
    RETURNING json
);

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1
    RETURNING jsonb
);


-- ============================================================
-- 084. SQL/JSON NULL HANDLING
-- ============================================================

SELECT JSON_ARRAY(
    1,
    NULL,
    2,
    NULL ON NULL
);

SELECT JSON_ARRAY(
    1,
    NULL,
    2,
    ABSENT ON NULL
);

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1,
    KEY 'b' VALUE NULL,
    NULL ON NULL
);

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1,
    KEY 'b' VALUE NULL
    ABSENT ON NULL
);


-- ============================================================
-- 085. SQL/JSON UNIQUE KEYS
-- ============================================================

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1,
    KEY 'b' VALUE 2
    WITH UNIQUE KEYS
);

SELECT JSON_OBJECT(
    KEY 'a' VALUE 1,
    KEY 'a' VALUE 2
    WITHOUT UNIQUE KEYS
);

DO $$
BEGIN
    PERFORM JSON_OBJECT(
        KEY 'a' VALUE 1,
        KEY 'a' VALUE 2
        WITH UNIQUE KEYS
    );
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED UNIQUE KEY ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 086. JSON_ARRAYAGG
-- ============================================================

SELECT JSON_ARRAYAGG(x)
FROM (
    VALUES (1),(2),(3)
) s(x);

SELECT JSON_ARRAYAGG(x ORDER BY x DESC)
FROM (
    VALUES (1),(2),(3)
) s(x);

SELECT JSON_ARRAYAGG(x)
FILTER (WHERE x > 1)
FROM (
    VALUES (1),(2),(3)
) s(x);

SELECT JSON_ARRAYAGG(x)
FROM (
    SELECT 1 AS x
    WHERE FALSE
) s;

SELECT JSON_ARRAYAGG(x NULL ON NULL)
FROM (
    VALUES (1),(NULL),(2)
) s(x);

SELECT JSON_ARRAYAGG(x ABSENT ON NULL)
FROM (
    VALUES (1),(NULL),(2)
) s(x);


-- ============================================================
-- 087. JSON_OBJECTAGG
-- ============================================================

SELECT JSON_OBJECTAGG(
    KEY k VALUE v
)
FROM (
    VALUES
        ('a',1),
        ('b',2)
) s(k,v);

SELECT JSON_OBJECTAGG(
    KEY k VALUE v
    ORDER BY k
)
FROM (
    VALUES
        ('b',2),
        ('a',1)
) s(k,v);

SELECT JSON_OBJECTAGG(
    KEY k VALUE v
    ABSENT ON NULL
)
FROM (
    VALUES
        ('a',1),
        ('b',NULL)
) s(k,v);

SELECT JSON_OBJECTAGG(
    KEY k VALUE v
    NULL ON NULL
)
FROM (
    VALUES
        ('a',1),
        ('b',NULL)
) s(k,v);


-- ============================================================
-- 088. JSON_SERIALIZE
-- ============================================================

SELECT JSON_SERIALIZE(
    JSON '{"a":1}'
);

SELECT JSON_SERIALIZE(
    JSON '{"a":1}'
    RETURNING TEXT
);

SELECT JSON_SERIALIZE(
    JSON '{"a":1}'
    RETURNING VARCHAR
);

SELECT JSON_SERIALIZE(
    JSON '[1,2,3]'
    RETURNING TEXT
);

SELECT JSON_SERIALIZE(
    JSON '"hello"'
    RETURNING TEXT
);

SELECT JSON_SERIALIZE(
    JSON 'null'
    RETURNING TEXT
);


-- ============================================================
-- 089. JSON_SCALAR TYPE COVERAGE
-- ============================================================

SELECT JSON_SCALAR(0);

SELECT JSON_SCALAR(-1);

SELECT JSON_SCALAR(123.456::numeric);

SELECT JSON_SCALAR('hello'::text);

SELECT JSON_SCALAR(true);

SELECT JSON_SCALAR(false);

SELECT JSON_SCALAR(
    DATE '2026-01-01'
);

SELECT JSON_SCALAR(
    TIMESTAMP '2026-01-01 12:30:45'
);

SELECT JSON_SCALAR(
    TIMESTAMPTZ '2026-01-01 12:30:45+05:30'
);

SELECT JSON_SCALAR(
    '550e8400-e29b-41d4-a716-446655440000'::uuid
);


-- ============================================================
-- 090. JSON_VALUE EMPTY / ERROR CLAUSES
-- ============================================================

SELECT JSON_VALUE(
    '{"x":1}',
    '$.x'
    RETURNING INTEGER
    ERROR ON ERROR
);

SELECT JSON_VALUE(
    '{"x":"bad"}',
    '$.x'
    RETURNING INTEGER
    DEFAULT 99 ON ERROR
);

SELECT JSON_VALUE(
    '{}',
    '$.x'
    RETURNING TEXT
    DEFAULT 'missing' ON EMPTY
);

SELECT JSON_VALUE(
    '{}',
    '$.x'
    RETURNING TEXT
    NULL ON EMPTY
);

SELECT JSON_VALUE(
    '{"x":[1,2]}',
    '$.x'
    RETURNING TEXT
    DEFAULT 'not-scalar' ON ERROR
);


-- ============================================================
-- 091. JSON_QUERY WRAPPER BEHAVIOR
-- ============================================================

SELECT JSON_QUERY(
    '{"x":[1,2,3]}',
    '$.x'
    WITHOUT WRAPPER
);

SELECT JSON_QUERY(
    '{"x":[1,2,3]}',
    '$.x'
    WITH WRAPPER
);

SELECT JSON_QUERY(
    '{"x":[1,2,3]}',
    '$.x[*]'
    WITH WRAPPER
);

SELECT JSON_QUERY(
    '{"x":[1,2,3]}',
    '$.x[*]'
    WITHOUT WRAPPER
);


-- ============================================================
-- 092. JSON_QUERY EMPTY / ERROR
-- ============================================================

SELECT JSON_QUERY(
    '{}',
    '$.missing'
    NULL ON EMPTY
);

SELECT JSON_QUERY(
    '{}',
    '$.missing'
    EMPTY ARRAY ON EMPTY
);

SELECT JSON_QUERY(
    '{}',
    '$.missing'
    EMPTY OBJECT ON EMPTY
);

SELECT JSON_QUERY(
    '{"x":123}',
    '$.x'
    EMPTY ARRAY ON ERROR
);

SELECT JSON_QUERY(
    '{"x":123}',
    '$.x'
    EMPTY OBJECT ON ERROR
);


-- ============================================================
-- 093. JSON_EXISTS ERROR BEHAVIOR
-- ============================================================

SELECT JSON_EXISTS(
    '{"x":1}',
    '$.x'
    TRUE ON ERROR
);

SELECT JSON_EXISTS(
    '{"x":1}',
    '$.missing'
    FALSE ON ERROR
);

SELECT JSON_EXISTS(
    '{"x":1}',
    '$.missing'
    UNKNOWN ON ERROR
);


-- ============================================================
-- 094. JSON_TABLE FOR ORDINALITY
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"name":"A"},
        {"name":"B"},
        {"name":"C"}
    ]',
    '$[*]'
    COLUMNS (
        rn FOR ORDINALITY,
        name TEXT PATH '$.name'
    )
) AS jt;


-- ============================================================
-- 095. JSON_TABLE EXISTS COLUMNS
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"active":true},
        {"id":2},
        {"id":3,"active":false}
    ]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        has_active BOOLEAN EXISTS PATH '$.active'
    )
) AS jt;


-- ============================================================
-- 096. JSON_TABLE JSON / QUERY COLUMNS
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {
            "id":1,
            "tags":["sql","json"]
        },
        {
            "id":2,
            "tags":["postgresql"]
        }
    ]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        tags JSON FORMAT JSON PATH '$.tags'
    )
) AS jt;


-- ============================================================

-- ============================================================
-- 096B. JSON_TABLE FORMAT JSON REGRESSION
-- ============================================================
-- A JSON-typed (FORMAT JSON) column must preserve the extracted value as
-- JSON: a JSON array must round-trip as ["sql","json"], not as the
-- PostgreSQL SQL-array text format {sql,json}.

SELECT *
FROM JSON_TABLE(
    '[{"id":1,"tags":["sql","json"]},{"id":2,"tags":["postgresql"]}]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        tags JSON FORMAT JSON PATH '$.tags'
    )
) AS jt;


-- 097. JSON_TABLE DEFAULT
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"name":"Alice"},
        {"id":2}
    ]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        name TEXT PATH '$.name'
            DEFAULT '"UNKNOWN"' ON EMPTY
    )
) AS jt;


-- ============================================================

-- ============================================================
-- 097B. JSON_TABLE DEFAULT ON ERROR REGRESSION
-- ============================================================
-- Conversion errors must apply the ON ERROR default; valid conversions
-- succeed; missing paths apply ON EMPTY. EMPTY and ERROR are distinct cases.

SELECT *
FROM JSON_TABLE(
    '[{"id":"abc"},{"id":"42"}]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id'
            DEFAULT '999' ON ERROR
    )
) AS jt;

SELECT *
FROM JSON_TABLE(
    '[{"id":"abc"},{"name":"Alice"}]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id'
            DEFAULT '999' ON EMPTY
            DEFAULT '888' ON ERROR
    )
) AS jt;


-- 098. JSON_TABLE NESTED OUTER BEHAVIOR
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {
            "id":1,
            "orders":[
                {"id":101},
                {"id":102}
            ]
        },
        {
            "id":2
        }
    ]',
    '$[*]'
    COLUMNS (
        customer_id INTEGER PATH '$.id',
        NESTED PATH '$.orders[*]'
        COLUMNS (
            order_id INTEGER PATH '$.id'
        )
    )
) AS jt;


-- ============================================================

-- ============================================================
-- 098B. JSON_TABLE NESTED EMPTY / MISSING REGRESSION
-- ============================================================
-- PostgreSQL INNER semantics for NESTED PATH: an empty nested array or a
-- missing nested array yields no row for that document, while matched
-- nested elements still expand.

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"orders":[{"id":101},{"id":102}]},
        {"id":2,"orders":[]},
        {"id":3}
    ]',
    '$[*]'
    COLUMNS (
        customer_id INTEGER PATH '$.id',
        NESTED PATH '$.orders[*]' COLUMNS (
            order_id INTEGER PATH '$.id'
        )
    )
) AS jt;


-- 099. JSONPATH OPERATORS
-- ============================================================

SELECT
    '{"a":10}'::jsonb @? '$.a';

SELECT
    '{"a":10}'::jsonb @? '$.missing';

SELECT
    '{"a":10}'::jsonb @@ '$.a == 10';

SELECT
    '{"a":10}'::jsonb @@ '$.a > 5';

SELECT
    '{"a":10}'::jsonb @@ '$.a < 5';


-- ============================================================
-- 100. JSONPATH METHODS
-- ============================================================

SELECT jsonb_path_query(
    '{"a":[1,2,3]}'::jsonb,
    '$.a.size()'
);

SELECT jsonb_path_query(
    '{"a":[1,2,3]}'::jsonb,
    '$.a.type()'
);

SELECT jsonb_path_query(
    '{"a":"hello"}'::jsonb,
    '$.a.type()'
);

SELECT jsonb_path_query(
    '{"a":123}'::jsonb,
    '$.a.type()'
);


-- ============================================================
-- 101. JSONPATH ARITHMETIC
-- ============================================================

SELECT jsonb_path_query(
    '{"a":10,"b":5}'::jsonb,
    '$.a + $.b'
);

SELECT jsonb_path_query(
    '{"a":10,"b":5}'::jsonb,
    '$.a - $.b'
);

SELECT jsonb_path_query(
    '{"a":10,"b":5}'::jsonb,
    '$.a * $.b'
);

SELECT jsonb_path_query(
    '{"a":10,"b":5}'::jsonb,
    '$.a / $.b'
);

SELECT jsonb_path_query(
    '{"a":10,"b":3}'::jsonb,
    '$.a % $.b'
);


-- ============================================================
-- 102. JSONPATH STRING METHODS
-- ============================================================

SELECT jsonb_path_query(
    '{"name":"PLOMID"}'::jsonb,
    '$.name'
);

SELECT jsonb_path_query(
    '{"name":"PLOMID"}'::jsonb,
    '$.name.starts_with("PLO")'
);

SELECT jsonb_path_query(
    '{"name":"PLOMID"}'::jsonb,
    '$.name like_regex "PLO.*"'
);

SELECT jsonb_path_query(
    '{"name":"PLOMID"}'::jsonb,
    '$.name flag "i"'
);


-- ============================================================
-- 103. JSONPATH ARRAY METHODS
-- ============================================================

SELECT jsonb_path_query(
    '[1,2,3]'::jsonb,
    '$.size()'
);

SELECT jsonb_path_query(
    '[1,2,3]'::jsonb,
    '$[0]'
);

SELECT jsonb_path_query(
    '[1,2,3]'::jsonb,
    '$[-1]'
);

SELECT jsonb_path_query(
    '[1,2,3]'::jsonb,
    '$[*]'
);


-- ============================================================
-- 104. JSONPATH OBJECT METHODS
-- ============================================================

SELECT jsonb_path_query(
    '{"a":1,"b":2}'::jsonb,
    '$.*'
);

SELECT jsonb_path_query(
    '{"a":1,"b":2}'::jsonb,
    '$.keyvalue()'
);


-- ============================================================
-- 105. JSONPATH DATETIME
-- ============================================================

SELECT jsonb_path_query_tz(
    '{"date":"2026-01-01"}'::jsonb,
    '$.date.datetime()'
);

SELECT jsonb_path_query_tz(
    '{"date":"2026-01-01T12:30:00+05:30"}'::jsonb,
    '$.date.datetime()'
);


-- ============================================================
-- 106. JSONPATH REGEX
-- ============================================================

SELECT jsonb_path_exists(
    '{"name":"PLOMID"}'::jsonb,
    '$.name like_regex "^PLO.*"'
);

SELECT jsonb_path_exists(
    '{"name":"plomid"}'::jsonb,
    '$.name like_regex "^PLO.*" flag "i"'
);

SELECT jsonb_path_exists(
    '{"name":"TEST"}'::jsonb,
    '$.name like_regex "^PLO.*"'
);


-- ============================================================
-- 107. JSONPATH NULL / UNKNOWN
-- ============================================================

SELECT jsonb_path_query(
    '{"a":null}'::jsonb,
    '$.a'
);

SELECT jsonb_path_exists(
    '{"a":null}'::jsonb,
    '$.a'
);

SELECT jsonb_path_match(
    '{"a":null}'::jsonb,
    '$.a == null'
);

SELECT jsonb_path_match(
    '{}'::jsonb,
    '$.missing == null'
);


-- ============================================================
-- 108. JSONPATH LAX STRUCTURAL BEHAVIOR
-- ============================================================

SELECT jsonb_path_query(
    '{"a":{"b":1}}'::jsonb,
    'lax $.a.b'
);

SELECT jsonb_path_query(
    '{"a":[{"b":1},{"b":2}]}'::jsonb,
    'lax $.a.b'
);

SELECT jsonb_path_query(
    '{"a":[1,2,3]}'::jsonb,
    'lax $.a.b'
);


-- ============================================================
-- 109. JSONPATH STRICT STRUCTURAL BEHAVIOR
-- ============================================================

DO $$
BEGIN
    PERFORM jsonb_path_query(
        '{"a":[1,2,3]}'::jsonb,
        'strict $.a.b'
    );
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED STRICT JSONPATH ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 110. JSONB CONCATENATION TYPE MATRIX
-- ============================================================

SELECT '1'::jsonb || '2'::jsonb;

SELECT '"a"'::jsonb || '"b"'::jsonb;

SELECT '1'::jsonb || '[2,3]'::jsonb;

SELECT '[1,2]'::jsonb || '3'::jsonb;

SELECT '"a"'::jsonb || '{"x":1}'::jsonb;

SELECT '{"x":1}'::jsonb || '"a"'::jsonb;

SELECT 'true'::jsonb || 'false'::jsonb;


-- ============================================================
-- 111. JSONB DELETION TYPE MATRIX
-- ============================================================

SELECT '{"a":1}'::jsonb - 'a';

SELECT '{"a":1}'::jsonb - 'missing';

SELECT '[1,2,3]'::jsonb - 0;

SELECT '[1,2,3]'::jsonb - -1;

DO $$
BEGIN
    PERFORM '"hello"'::jsonb - 'x';
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED JSONB DELETE TYPE ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 112. JSONB SUBSCRIPT MISSING PATH
-- ============================================================

SELECT
    '{}'::jsonb['a']['b'];

SELECT
    '{"a":{}}'::jsonb['a']['b'];

SELECT
    '{"a":[]}'::jsonb['a'][0];

SELECT
    '{"a":[1,2]}'::jsonb['a'][99];


-- ============================================================
-- 113. JSON CONVERSION ROUND-TRIP TYPES
-- ============================================================

SELECT to_jsonb(ARRAY[DATE '2026-01-01']);

SELECT to_jsonb(
    ARRAY[
        TIMESTAMP '2026-01-01 10:00:00',
        TIMESTAMP '2026-01-02 11:00:00'
    ]
);

SELECT to_jsonb(
    ARRAY[
        '550e8400-e29b-41d4-a716-446655440000'::uuid
    ]
);

SELECT to_jsonb(
    ARRAY[
        '127.0.0.1'::inet,
        '192.168.1.1'::inet
    ]
);


-- ============================================================
-- 114. COMPOSITE NULL BEHAVIOR
-- ============================================================

SELECT to_json(
    ROW(
        NULL::INTEGER,
        NULL::TEXT,
        NULL::BOOLEAN
    )
);

SELECT to_jsonb(
    ROW(
        NULL::INTEGER,
        NULL::TEXT,
        NULL::BOOLEAN
    )
);


-- ============================================================
-- 115. DOMAIN CONVERSION
-- ============================================================

DROP DOMAIN IF EXISTS json_gap_081.text_domain CASCADE;

CREATE DOMAIN json_gap_081.text_domain AS TEXT;

SELECT to_json(
    'PLOMID'::json_gap_081.text_domain
);

SELECT to_jsonb(
    'PLOMID'::json_gap_081.text_domain
);


-- ============================================================
-- 116. ENUM CONVERSION
-- ============================================================

DROP TYPE IF EXISTS json_gap_081.status_enum CASCADE;

CREATE TYPE json_gap_081.status_enum AS ENUM (
    'pending',
    'active',
    'completed'
);

SELECT to_json(
    'pending'::json_gap_081.status_enum
);

SELECT to_jsonb(
    'completed'::json_gap_081.status_enum
);

SELECT to_jsonb(
    ARRAY[
        'pending'::json_gap_081.status_enum,
        'active'::json_gap_081.status_enum
    ]
);


-- ============================================================
-- 117. RANGE / MULTIRANGE CONVERSION
-- ============================================================

SELECT to_json(
    int4range(1,10)
);

SELECT to_jsonb(
    int4range(1,10)
);

SELECT to_json(
    int4multirange(
        int4range(1,5),
        int4range(10,20)
    )
);

SELECT to_jsonb(
    int4multirange(
        int4range(1,5),
        int4range(10,20)
    )
);


-- ============================================================
-- 118. MONEY / BIT / XML
-- ============================================================

SELECT to_json(
    123.45::money
);

SELECT to_jsonb(
    123.45::money
);

SELECT to_json(
    B'101010'::bit(6)
);

SELECT to_jsonb(
    B'101010'::bit(6)
);

SELECT to_json(
    '<root><x>1</x></root>'::xml
);

SELECT to_jsonb(
    '<root><x>1</x></root>'::xml
);


-- ============================================================
-- 119. JSONB HASH / EQUALITY EDGE CASES
-- ============================================================

SELECT
    '{"a":1,"b":2}'::jsonb =
    '{"b":2,"a":1}'::jsonb;

SELECT
    '[1,2,3]'::jsonb =
    '[1,2,3]'::jsonb;

SELECT
    '[1,2,3]'::jsonb =
    '[3,2,1]'::jsonb;

SELECT
    '{"a":1}'::jsonb =
    '{"a":1.0}'::jsonb;

SELECT
    '{"a":1}'::jsonb =
    '{"a":1.00}'::jsonb;


-- ============================================================
-- 120. JSONB SORT ORDER
-- ============================================================

SELECT value
FROM (
    VALUES
        ('null'::jsonb),
        ('false'::jsonb),
        ('true'::jsonb),
        ('1'::jsonb),
        ('2'::jsonb),
        ('"a"'::jsonb),
        ('"b"'::jsonb),
        ('[]'::jsonb),
        ('[1]'::jsonb),
        ('{}'::jsonb),
        ('{"a":1}'::jsonb)
) s(value)
ORDER BY value;


-- ============================================================
-- 121. AGGREGATE FILTER + ORDER
-- ============================================================

SELECT jsonb_agg(
    value
    ORDER BY value DESC
)
FILTER (
    WHERE value >= 2
)
FROM (
    VALUES
        (1),(2),(3),(4)
) s(value);

SELECT JSON_ARRAYAGG(
    value
    ORDER BY value
)
FILTER (
    WHERE value IS NOT NULL
)
FROM (
    VALUES
        (3),(NULL),(1),(2)
) s(value);


-- ============================================================
-- 122. AGGREGATE NULL VARIANTS
-- ============================================================

SELECT jsonb_agg(value)
FROM (
    VALUES
        (NULL),
        (NULL)
) s(value);

SELECT JSON_ARRAYAGG(value NULL ON NULL)
FROM (
    VALUES
        (NULL),
        (NULL)
) s(value);

SELECT JSON_ARRAYAGG(value ABSENT ON NULL)
FROM (
    VALUES
        (NULL),
        (NULL)
) s(value);


-- ============================================================
-- 123. OBJECT AGGREGATE DUPLICATE KEYS
-- ============================================================

SELECT jsonb_object_agg(
    key,
    value
)
FROM (
    VALUES
        ('a','1'),
        ('a','2')
) s(key,value);

SELECT json_object_agg(
    key,
    value
)
FROM (
    VALUES
        ('a','1'),
        ('a','2')
) s(key,value);


-- ============================================================
-- 124. SQL/JSON DUPLICATE KEY BEHAVIOR
-- ============================================================

DO $$
BEGIN
    PERFORM JSON_OBJECT(
        KEY 'a' VALUE 1,
        KEY 'a' VALUE 2
        WITH UNIQUE KEYS
    );
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED SQL/JSON DUPLICATE KEY ERROR: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 125. JSON_TABLE MULTI-LEVEL NESTING
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '{
        "departments":[
            {
                "name":"Engineering",
                "teams":[
                    {
                        "name":"Backend",
                        "members":[
                            {"id":1,"name":"Alice"},
                            {"id":2,"name":"Bob"}
                        ]
                    },
                    {
                        "name":"Frontend",
                        "members":[
                            {"id":3,"name":"Carol"}
                        ]
                    }
                ]
            }
        ]
    }',
    '$.departments[*]'
    COLUMNS (
        department_name TEXT PATH '$.name',

        NESTED PATH '$.teams[*]'
        COLUMNS (
            team_name TEXT PATH '$.name',

            NESTED PATH '$.members[*]'
            COLUMNS (
                member_id INTEGER PATH '$.id',
                member_name TEXT PATH '$.name'
            )
        )
    )
) AS jt;


-- ============================================================
-- 126. JSON_TABLE ORDINALITY + EXISTS
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"tags":["sql"]},
        {"id":2},
        {"id":3,"tags":[]}
    ]',
    '$[*]'
    COLUMNS (
        row_number FOR ORDINALITY,
        id INTEGER PATH '$.id',
        has_tags BOOLEAN EXISTS PATH '$.tags'
    )
) AS jt;


-- ============================================================
-- 127. JSONPATH VARIABLES ARRAYS / OBJECTS
-- ============================================================

SELECT jsonb_path_exists(
    '{"tags":["sql","json","postgresql"]}'::jsonb,
    '$.tags[*] == any ($wanted)',
    '{"wanted":["json","xml"]}'::jsonb
);

SELECT jsonb_path_query(
    '{"a":10,"b":20,"c":30}'::jsonb,
    '$.* ? (@ >= $min)',
    '{"min":20}'::jsonb
);


-- ============================================================
-- 128. JSONPATH COMPARISON MATRIX
-- ============================================================

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a == 10'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a != 10'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a >= 10'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a <= 10'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a > 9'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a < 11'
);


-- ============================================================
-- 129. JSONB OPERATOR-CLASS COVERAGE
-- ============================================================

DROP TABLE IF EXISTS json_gap_081.operator_test;

CREATE TABLE json_gap_081.operator_test (
    id INTEGER PRIMARY KEY,
    payload JSONB
);

INSERT INTO json_gap_081.operator_test
SELECT
    i,
    jsonb_build_object(
        'id', i,
        'active', i % 2 = 0,
        'group', i % 5,
        'tags', jsonb_build_array(
            'sql',
            CASE
                WHEN i % 2 = 0
                THEN 'even'
                ELSE 'odd'
            END
        )
    )
FROM generate_series(1,100) i;

CREATE INDEX operator_test_gin_default
ON json_gap_081.operator_test
USING GIN(payload);

CREATE INDEX operator_test_gin_path
ON json_gap_081.operator_test
USING GIN(payload jsonb_path_ops);

EXPLAIN
SELECT *
FROM json_gap_081.operator_test
WHERE payload @> '{"active":true}'::jsonb;


-- ============================================================
-- 130. EXPRESSION INDEX VARIANTS
-- ============================================================

CREATE INDEX operator_test_group_idx
ON json_gap_081.operator_test(
    ((payload ->> 'group')::INTEGER)
);

CREATE INDEX operator_test_active_idx
ON json_gap_081.operator_test(
    (payload ->> 'active')
);

EXPLAIN
SELECT *
FROM json_gap_081.operator_test
WHERE (payload ->> 'group')::INTEGER = 3;

EXPLAIN
SELECT *
FROM json_gap_081.operator_test
WHERE payload ->> 'active' = 'true';


-- ============================================================
-- 131. GENERATED JSONB MUTATION WORKLOAD
-- ============================================================

DROP TABLE IF EXISTS json_gap_081.generated_workload;

CREATE TABLE json_gap_081.generated_workload (
    id INTEGER PRIMARY KEY,
    payload JSONB NOT NULL,
    customer_id INTEGER
        GENERATED ALWAYS AS (
            (payload ->> 'customer_id')::INTEGER
        ) STORED,
    active BOOLEAN
        GENERATED ALWAYS AS (
            (payload ->> 'active')::BOOLEAN
        ) STORED
);

INSERT INTO json_gap_081.generated_workload(
    id,
    payload
)
VALUES
(
    1,
    '{"customer_id":1001,"active":true}'
),
(
    2,
    '{"customer_id":1002,"active":false}'
);

UPDATE json_gap_081.generated_workload
SET payload = jsonb_set(
    payload,
    '{customer_id}',
    '2001'::jsonb
)
WHERE id = 1;

SELECT *
FROM json_gap_081.generated_workload
ORDER BY id;


-- ============================================================
-- 132. JSONB STATISTICS EXPRESSION WORKLOAD
-- ============================================================

ANALYZE json_gap_081.operator_test;

EXPLAIN
SELECT count(*)
FROM json_gap_081.operator_test
WHERE (payload ->> 'group')::INTEGER = 3;

EXPLAIN
SELECT count(*)
FROM json_gap_081.operator_test
WHERE payload @> '{"group":3}'::jsonb;

EXPLAIN
SELECT count(*)
FROM json_gap_081.operator_test
WHERE payload ? 'active';


-- ============================================================
-- 133. LARGE NESTED EXTRACTION
-- ============================================================

WITH documents AS (
    SELECT jsonb_build_object(
        'id',
        i,
        'customer',
        jsonb_build_object(
            'profile',
            jsonb_build_object(
                'name',
                'Customer ' || i,
                'attributes',
                jsonb_build_object(
                    'region','IN',
                    'tier',
                    CASE
                        WHEN i % 2 = 0
                        THEN 'enterprise'
                        ELSE 'standard'
                    END
                )
            )
        )
    ) AS payload
    FROM generate_series(1,1000) i
)
SELECT count(*)
FROM documents
WHERE payload #>> '{customer,profile,attributes,tier}'
      = 'enterprise';


-- ============================================================
-- 134. LARGE NESTED MUTATION
-- ============================================================

WITH documents AS (
    SELECT jsonb_build_object(
        'id',
        i,
        'customer',
        jsonb_build_object(
            'profile',
            jsonb_build_object(
                'name',
                'Customer ' || i
            )
        )
    ) AS payload
    FROM generate_series(1,100) i
)
SELECT count(*)
FROM documents
WHERE jsonb_set(
    payload,
    '{customer,profile,status}',
    '"active"'::jsonb,
    true
) -> 'customer' -> 'profile' ->> 'status'
= 'active';


-- ============================================================
-- 135. UNICODE ESCAPE EDGE CASES
-- ============================================================

SELECT
    E'{"text":"\\u0041"}'::jsonb;

SELECT
    E'{"text":"\\u00E9"}'::jsonb;

SELECT
    E'{"text":"\\u4F60\\u597D"}'::jsonb;

SELECT
    E'{"text":"\\uD83D\\uDE00"}'::jsonb;

SELECT jsonb_build_object(
    'text',
    E'\\u0041'
);


-- ============================================================
-- 136. JSON NUMBER LEXICAL EDGE CASES
-- ============================================================

SELECT '0'::jsonb;

SELECT '-0'::jsonb;

SELECT '0.0'::jsonb;

SELECT '-0.0'::jsonb;

SELECT '1.0'::jsonb;

SELECT '1e0'::jsonb;

SELECT '1E+10'::jsonb;

SELECT '1E-10'::jsonb;

DO $$
BEGIN
    PERFORM '01'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED INVALID JSON NUMBER: %',
            SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM '.1'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE
            'EXPECTED INVALID JSON NUMBER: %',
            SQLERRM;
END
$$;


-- ============================================================
-- 137. JSON STRING LEXICAL EDGE CASES
-- ============================================================

SELECT '"simple"'::jsonb;

SELECT '"hello world"'::jsonb;

SELECT '"hello\nworld"'::jsonb;

SELECT '"hello\tworld"'::jsonb;

SELECT '"quote: \"hello\""'::jsonb;

SELECT '"slash: \\\\"'::jsonb;


-- ============================================================
-- 138. EMPTY / NULL SQL INPUTS
-- ============================================================

SELECT to_json(NULL::TEXT);

SELECT to_jsonb(NULL::TEXT);

SELECT json_build_array(NULL::TEXT);

SELECT jsonb_build_array(NULL::TEXT);

SELECT json_build_object(
    'value',
    NULL::TEXT
);

SELECT jsonb_build_object(
    'value',
    NULL::TEXT
);

SELECT json_agg(NULL::TEXT);

SELECT jsonb_agg(NULL::TEXT);

SELECT JSON_SCALAR(NULL);

SELECT JSON_VALUE(
    NULL,
    '$.x'
);

SELECT JSON_QUERY(
    NULL,
    '$.x'
);

SELECT JSON_EXISTS(
    NULL,
    '$.x'
);


-- ============================================================
-- 139. FINAL GAP INTEGRATION
-- ============================================================

WITH source AS (
    SELECT
        i,
        jsonb_build_object(
            'id', i,
            'customer', jsonb_build_object(
                'id', 1000 + i,
                'name', 'Customer ' || i
            ),
            'active', i % 2 = 0,
            'tags', jsonb_build_array(
                'sql',
                'json',
                CASE
                    WHEN i % 2 = 0
                    THEN 'enterprise'
                    ELSE 'standard'
                END
            )
        ) AS payload
    FROM generate_series(1,20) i
)
SELECT
    count(*) AS total_rows,
    count(*) FILTER (
        WHERE payload ->> 'active' = 'true'
    ) AS active_rows,
    jsonb_agg(
        payload ->> 'id'
        ORDER BY i
    ) AS ids,
    jsonb_agg(
        payload -> 'customer'
        ORDER BY i
    ) AS customers
FROM source;


SELECT
    jsonb_path_query_array(
        jsonb_agg(payload),
        '$[*] ? (@.active == true).customer'
    )
FROM (
    SELECT jsonb_build_object(
        'id', id,
        'active', id % 2 = 0,
        'customer',
        jsonb_build_object(
            'id', id + 1000
        )
    ) AS payload
    FROM generate_series(1,10) id
) s;


SELECT
    jsonb_object_agg(
        id::TEXT,
        payload
        ORDER BY id
    )
FROM (
    SELECT
        id,
        jsonb_build_object(
            'id', id,
            'status',
            CASE
                WHEN id % 2 = 0
                THEN 'active'
                ELSE 'pending'
            END
        ) AS payload
    FROM generate_series(1,10) id
);



-- ============================================================
-- END OF POSTGRESQL 17 JSON / JSONB ENTERPRISE SUITE
-- ============================================================
