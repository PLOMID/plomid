-- ============================================================
-- PLOMID
-- JSON / JSONB GAP + HARDENING TEST SUITE
--
-- Target: PostgreSQL 17 compatibility
--
-- Purpose:
--   Additional coverage beyond JSON_ALL.sql.
--
-- This suite focuses on:
--   1. Complete JSONB operator matrix
--   2. Scalar/type matrix
--   3. String escape matrix
--   4. Numeric extremes
--   5. Unicode edge cases
--   6. Nested document query matrix
--   7. Nested mutation matrix
--   8. JSONPath hardening
--   9. JSON_TABLE hardening
--  10. Cast / round-trip behavior
--  11. Large nested documents
--  12. Final integration workloads
--
-- IMPORTANT:
--   PostgreSQL-native syntax is intentional.
--
-- ERROR POLICY:
--   ON_ERROR_STOP is disabled so negative tests continue.
-- ============================================================

\set ON_ERROR_STOP off

SET client_min_messages = NOTICE;

DROP SCHEMA IF EXISTS json_gap CASCADE;
CREATE SCHEMA json_gap;


-- ============================================================
-- 001. BASE JSON / JSONB SCALAR MATRIX
-- ============================================================

SELECT 'null'::json;
SELECT 'true'::json;
SELECT 'false'::json;
SELECT '0'::json;
SELECT '-0'::json;
SELECT '123'::json;
SELECT '-123'::json;
SELECT '123.456'::json;
SELECT '1e10'::json;
SELECT '1e-10'::json;
SELECT '"hello"'::json;

SELECT 'null'::jsonb;
SELECT 'true'::jsonb;
SELECT 'false'::jsonb;
SELECT '0'::jsonb;
SELECT '-0'::jsonb;
SELECT '123'::jsonb;
SELECT '-123'::jsonb;
SELECT '123.456'::jsonb;
SELECT '1e10'::jsonb;
SELECT '1e-10'::jsonb;
SELECT '"hello"'::jsonb;


-- ============================================================
-- 002. JSONB TYPE INSPECTION
-- ============================================================

SELECT jsonb_typeof('null'::jsonb);
SELECT jsonb_typeof('true'::jsonb);
SELECT jsonb_typeof('false'::jsonb);
SELECT jsonb_typeof('123'::jsonb);
SELECT jsonb_typeof('123.45'::jsonb);
SELECT jsonb_typeof('"hello"'::jsonb);
SELECT jsonb_typeof('[]'::jsonb);
SELECT jsonb_typeof('{}'::jsonb);


-- ============================================================
-- 003. COMPLETE JSONB OPERATOR MATRIX
-- ============================================================

SELECT
    '{"a":{"b":123},"tags":["sql","json"],"active":true}'::jsonb
        -> 'a';

SELECT
    '{"a":{"b":123},"tags":["sql","json"],"active":true}'::jsonb
        ->> 'active';

SELECT
    '{"a":{"b":123}}'::jsonb
        #> '{a,b}';

SELECT
    '{"a":{"b":123}}'::jsonb
        #>> '{a,b}';

SELECT
    '{"a":{"b":123}}'::jsonb
        @> '{"a":{"b":123}}'::jsonb;

SELECT
    '{"a":{"b":123}}'::jsonb
        <@ '{"a":{"b":123},"x":1}'::jsonb;

SELECT
    '{"a":1,"b":2}'::jsonb
        ? 'a';

SELECT
    '{"a":1,"b":2}'::jsonb
        ? 'missing';

SELECT
    '{"a":1,"b":2}'::jsonb
        ?| ARRAY['x','b'];

SELECT
    '{"a":1,"b":2}'::jsonb
        ?& ARRAY['a','b'];

SELECT
    '{"a":1}'::jsonb
        || '{"b":2}'::jsonb;

SELECT
    '[1,2]'::jsonb
        || '[3,4]'::jsonb;

SELECT
    '{"a":1,"b":2}'::jsonb
        - 'a';

SELECT
    '[1,2,3]'::jsonb
        - 1;

SELECT
    '{"a":{"b":1}}'::jsonb
        #- '{a,b}';


-- ============================================================
-- 004. OPERATOR CHAINING
-- ============================================================

SELECT
    '{"customer":{"profile":{"name":"Alice"}}}'::jsonb
        -> 'customer'
        -> 'profile'
        ->> 'name';

SELECT
    '{"customers":[{"id":1},{"id":2}]}'::jsonb
        -> 'customers'
        -> 1
        ->> 'id';

SELECT
    '{"a":{"b":[{"c":42}]}}'::jsonb
        #>> '{a,b,0,c}';


-- ============================================================
-- 005. ARRAY INDEX EDGE CASES
-- ============================================================

SELECT '[10,20,30]'::jsonb -> 0;
SELECT '[10,20,30]'::jsonb -> 1;
SELECT '[10,20,30]'::jsonb -> 2;
SELECT '[10,20,30]'::jsonb -> -1;
SELECT '[10,20,30]'::jsonb -> -2;

SELECT '[10,20,30]'::jsonb ->> 0;
SELECT '[10,20,30]'::jsonb ->> -1;

SELECT '[10,20,30]'::jsonb #> '{-1}';
SELECT '[10,20,30]'::jsonb #>> '{-1}';


-- ============================================================
-- 006. STRING ESCAPE MATRIX
-- ============================================================

SELECT '"quote: \"hello\""'::jsonb;
SELECT '"backslash: \\"'::jsonb;
SELECT '"slash: \/"'::jsonb;
SELECT '"backspace: \b"'::jsonb;
SELECT '"formfeed: \f"'::jsonb;
SELECT '"newline: \n"'::jsonb;
SELECT '"carriage: \r"'::jsonb;
SELECT '"tab: \t"'::jsonb;

SELECT E'"unicode: \\u0041"'::jsonb;
SELECT E'"unicode: \\u00E9"'::jsonb;
SELECT E'"unicode: \\u4F60\\u597D"'::jsonb;
SELECT E'"unicode: \\uD83D\\uDE00"'::jsonb;


-- ============================================================
-- 007. INVALID STRING ESCAPES
-- ============================================================

DO $$
BEGIN
    PERFORM '"bad: \q"'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID ESCAPE: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM E'"bad unicode: \\u12"'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID UNICODE ESCAPE: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM E'"bad unicode: \\uZZZZ"'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID UNICODE ESCAPE: %', SQLERRM;
END
$$;


-- ============================================================
-- 008. NUMERIC EXTREMES
-- ============================================================

SELECT '0'::jsonb;
SELECT '-0'::jsonb;
SELECT '0.0'::jsonb;
SELECT '-0.0'::jsonb;

SELECT '1.0000000000000000000000001'::jsonb;
SELECT '-1.0000000000000000000000001'::jsonb;

SELECT '1e100'::jsonb;
SELECT '1e-100'::jsonb;
SELECT '-1e100'::jsonb;
SELECT '-1e-100'::jsonb;

SELECT '999999999999999999999999999999999999999999'::jsonb;


-- ============================================================
-- 009. INVALID JSON NUMBERS
-- ============================================================

DO $$
BEGIN
    PERFORM '01'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID NUMBER: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM '.1'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID NUMBER: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM '1.'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID NUMBER: %', SQLERRM;
END
$$;

DO $$
BEGIN
    PERFORM '1e'::jsonb;
EXCEPTION
    WHEN OTHERS THEN
        RAISE NOTICE 'EXPECTED INVALID NUMBER: %', SQLERRM;
END
$$;


-- ============================================================
-- 010. DUPLICATE KEYS
-- ============================================================

SELECT '{"a":1,"a":2}'::json;
SELECT '{"a":1,"a":2}'::jsonb;

SELECT '{"a":{"x":1,"x":2}}'::json;
SELECT '{"a":{"x":1,"x":2}}'::jsonb;

SELECT '{"items":[{"id":1,"id":2}]}'::jsonb;


-- ============================================================
-- 011. NULL VS MISSING
-- ============================================================

SELECT
    '{"a":null}'::jsonb -> 'a';

SELECT
    '{"a":null}'::jsonb ->> 'a';

SELECT
    '{"a":null}'::jsonb ? 'a';

SELECT
    '{"a":1}'::jsonb -> 'missing';

SELECT
    '{"a":1}'::jsonb ->> 'missing';

SELECT
    '{"a":1}'::jsonb ? 'missing';


-- ============================================================
-- 012. NESTED DOCUMENT QUERY MATRIX
-- ============================================================

CREATE TABLE json_gap.documents (
    id INTEGER PRIMARY KEY,
    payload JSONB
);

INSERT INTO json_gap.documents VALUES
(
    1,
    '{
        "customer": {
            "id": 100,
            "profile": {
                "name": "Alice",
                "age": 30
            }
        },
        "orders": [
            {
                "id": 1001,
                "items": [
                    {"sku":"A","qty":2},
                    {"sku":"B","qty":1}
                ]
            },
            {
                "id": 1002,
                "items": [
                    {"sku":"C","qty":5}
                ]
            }
        ],
        "tags": ["sql","json","postgresql"],
        "active": true
    }'
),
(
    2,
    '{
        "customer": {
            "id": 200,
            "profile": {
                "name": "Bob",
                "age": 40
            }
        },
        "orders": [],
        "tags": ["database"],
        "active": false
    }'
);


SELECT
    id,
    payload -> 'customer'
FROM json_gap.documents;

SELECT
    id,
    payload -> 'customer' -> 'profile'
FROM json_gap.documents;

SELECT
    id,
    payload -> 'customer' -> 'profile' ->> 'name'
FROM json_gap.documents;

SELECT
    id,
    payload #>> '{customer,profile,name}'
FROM json_gap.documents;

SELECT
    id,
    payload -> 'orders' -> 0
FROM json_gap.documents;

SELECT
    id,
    payload #>> '{orders,0,items,0,sku}'
FROM json_gap.documents;


-- ============================================================
-- 013. NESTED CONTAINMENT
-- ============================================================

SELECT *
FROM json_gap.documents
WHERE payload @> '{"active":true}'::jsonb;

SELECT *
FROM json_gap.documents
WHERE payload @> '{"customer":{"profile":{"name":"Alice"}}}'::jsonb;

SELECT *
FROM json_gap.documents
WHERE payload @> '{"tags":["json"]}'::jsonb;


-- ============================================================
-- 014. JSONB ARRAY EXPANSION
-- ============================================================

SELECT
    id,
    jsonb_array_elements(payload -> 'tags')
FROM json_gap.documents
ORDER BY id;

SELECT
    id,
    element
FROM json_gap.documents
CROSS JOIN LATERAL
    jsonb_array_elements(payload -> 'orders') AS element
ORDER BY id;


-- ============================================================
-- 015. NESTED ARRAY EXPANSION
-- ============================================================

SELECT
    d.id,
    order_doc,
    item_doc
FROM json_gap.documents d
CROSS JOIN LATERAL
    jsonb_array_elements(d.payload -> 'orders') AS order_doc
CROSS JOIN LATERAL
    jsonb_array_elements(order_doc -> 'items') AS item_doc
ORDER BY d.id;


-- ============================================================
-- 016. OBJECT EXPANSION
-- ============================================================

SELECT
    jsonb_each('{"a":1,"b":2,"c":3}'::jsonb);

SELECT
    jsonb_each_text('{"a":1,"b":2,"c":3}'::jsonb);

SELECT
    key,
    value
FROM json_gap.documents d
CROSS JOIN LATERAL
    jsonb_each(d.payload -> 'customer');


-- ============================================================
-- 017. JSONB MUTATION MATRIX
-- ============================================================

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
    '{"a":{}}'::jsonb,
    '{a,b,c}',
    '"deep"'::jsonb,
    true
);

SELECT jsonb_insert(
    '{"a":[1,3]}'::jsonb,
    '{a,1}',
    '2'::jsonb
);

SELECT jsonb_insert(
    '{"a":[1,2]}'::jsonb,
    '{a,0}',
    '0'::jsonb,
    true
);

SELECT
    '{"a":{"b":1,"c":2}}'::jsonb
    #- '{a,b}';

SELECT
    '{"a":1,"b":2}'::jsonb
    - 'a';

SELECT
    '{"a":1}'::jsonb
    || '{"b":2}'::jsonb;


-- ============================================================
-- 018. LARGE NESTED MUTATION
-- ============================================================

WITH documents AS (
    SELECT jsonb_build_object(
        'id', i,
        'customer',
        jsonb_build_object(
            'profile',
            jsonb_build_object(
                'name', 'Customer ' || i
            )
        )
    ) AS payload
    FROM generate_series(1,1000) i
)
SELECT count(*)
FROM documents
WHERE jsonb_set(
    payload,
    '{customer,profile,status}',
    '"active"'::jsonb,
    true
) -> 'customer' -> 'profile' ->> 'status' = 'active';


-- ============================================================
-- 019. JSONB CONSTRUCTION MATRIX
-- ============================================================

SELECT jsonb_build_object(
    'id', 1,
    'name', 'Alice',
    'active', true,
    'score', 42.5,
    'nothing', NULL
);

SELECT jsonb_build_array(
    1,
    'hello',
    true,
    NULL,
    '{"nested":true}'::jsonb
);

SELECT jsonb_build_object(
    'customer',
    jsonb_build_object(
        'id', 1,
        'profile',
        jsonb_build_object(
            'name', 'Alice'
        )
    )
);


-- ============================================================
-- 020. JSON / JSONB CAST ROUND TRIPS
-- ============================================================

SELECT '{}'::json::jsonb::json;
SELECT '{}'::jsonb::json::jsonb;

SELECT '[]'::json::jsonb::json;
SELECT '[]'::jsonb::json::jsonb;

SELECT
    '{"a":1,"b":[1,2,3]}'::json
    ::jsonb
    ::json;

SELECT
    '{"a":1,"b":[1,2,3]}'::jsonb
    ::json
    ::jsonb;


-- ============================================================
-- 021. JSONB EQUALITY / ORDERING
-- ============================================================

SELECT
    '{"a":1,"b":2}'::jsonb
    =
    '{"b":2,"a":1}'::jsonb;

SELECT
    '{"a":1,"b":2}'::jsonb
    <>
    '{"a":1,"b":3}'::jsonb;

SELECT
    '[1,2]'::jsonb
    <
    '[1,3]'::jsonb;

SELECT
    '{"a":1}'::jsonb
    =
    '{"a":1}'::jsonb;


-- ============================================================
-- 022. DISTINCT / GROUP BY
-- ============================================================

CREATE TABLE json_gap.group_test (
    id INTEGER,
    payload JSONB
);

INSERT INTO json_gap.group_test VALUES
    (1, '{"a":1,"b":2}'),
    (2, '{"b":2,"a":1}'),
    (3, '{"a":2}'),
    (4, '{"a":2}');

SELECT DISTINCT payload
FROM json_gap.group_test
ORDER BY payload;

SELECT payload, count(*)
FROM json_gap.group_test
GROUP BY payload
ORDER BY payload;


-- ============================================================
-- 023. JSONPATH BASIC HARDENING
-- ============================================================

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.a'
);

SELECT jsonb_path_exists(
    '{"a":10}'::jsonb,
    '$.missing'
);

SELECT jsonb_path_query(
    '{"a":10,"b":20,"c":30}'::jsonb,
    '$.*'
);

SELECT jsonb_path_query(
    '{"a":10,"b":20,"c":30}'::jsonb,
    '$.* ? (@ >= 20)'
);

SELECT jsonb_path_query_array(
    '{"a":[1,2,3,4]}'::jsonb,
    '$.a[*] ? (@ > 2)'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a == 10'
);

SELECT jsonb_path_match(
    '{"a":10}'::jsonb,
    '$.a > 5 && $.a < 20'
);


-- ============================================================
-- 024. JSONPATH VARIABLES
-- ============================================================

SELECT jsonb_path_exists(
    '{"price":100}'::jsonb,
    '$.price > $min',
    '{"min":50}'::jsonb
);

SELECT jsonb_path_query(
    '{"items":[10,20,30]}'::jsonb,
    '$.items[*] ? (@ > $min)',
    '{"min":15}'::jsonb
);


-- ============================================================
-- 025. JSONPATH ARRAY / OBJECT OPERATIONS
-- ============================================================

SELECT jsonb_path_query(
    '{"items":[{"id":1},{"id":2},{"id":3}]}'::jsonb,
    '$.items[*].id'
);

SELECT jsonb_path_query(
    '{"a":{"x":1,"y":2,"z":3}}'::jsonb,
    '$.a.*'
);


-- ============================================================
-- 026. JSON_TABLE BASIC
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[
        {"id":1,"name":"Alice"},
        {"id":2,"name":"Bob"}
    ]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id',
        name TEXT PATH '$.name'
    )
) AS jt;


-- ============================================================
-- 027. JSON_TABLE ORDINALITY
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
        row_number FOR ORDINALITY,
        name TEXT PATH '$.name'
    )
) AS jt;


-- ============================================================
-- 028. JSON_TABLE EXISTS
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
        id INTEGER PATH '$.id',
        has_tags BOOLEAN EXISTS PATH '$.tags'
    )
) AS jt;


-- ============================================================
-- 029. JSON_TABLE NESTED
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
            "id":2,
            "orders":[
                {"id":201}
            ]
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
-- 030. JSON_TABLE DEFAULTS
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
-- 031. JSON_TABLE ON ERROR
-- ============================================================

SELECT *
FROM JSON_TABLE(
    '[{"id":"abc"},{"id":"42"}]',
    '$[*]'
    COLUMNS (
        id INTEGER PATH '$.id'
            DEFAULT '999' ON ERROR
    )
) AS jt;


-- ============================================================
-- 032. AGGREGATE MATRIX
-- ============================================================

SELECT json_agg(v)
FROM (
    VALUES (1), (2), (3)
) AS t(v);

SELECT jsonb_agg(v)
FROM (
    VALUES (1), (2), (3)
) AS t(v);

SELECT json_object_agg(k, v)
FROM (
    VALUES
        ('a', '1'),
        ('b', '2')
) AS t(k,v);

SELECT jsonb_object_agg(k, v)
FROM (
    VALUES
        ('a', '1'),
        ('b', '2')
) AS t(k,v);


-- ============================================================
-- 033. EMPTY AGGREGATES
-- ============================================================

SELECT json_agg(v)
FROM (
    SELECT 1 AS v
    WHERE FALSE
) t;

SELECT jsonb_agg(v)
FROM (
    SELECT 1 AS v
    WHERE FALSE
) t;

SELECT json_object_agg(k,v)
FROM (
    SELECT 'a' AS k, '1' AS v
    WHERE FALSE
) t;

SELECT jsonb_object_agg(k,v)
FROM (
    SELECT 'a' AS k, '1' AS v
    WHERE FALSE
) t;


-- ============================================================
-- 034. ORDERED / FILTERED AGGREGATES
-- ============================================================

SELECT jsonb_agg(v ORDER BY v DESC)
FROM generate_series(1,5) v;

SELECT jsonb_agg(v ORDER BY v)
FROM generate_series(1,5) v
WHERE v % 2 = 0;


-- ============================================================
-- 035. RECORD / DOCUMENT CONVERSION
-- ============================================================

CREATE TABLE json_gap.users (
    id INTEGER,
    name TEXT,
    active BOOLEAN
);

INSERT INTO json_gap.users VALUES
    (1, 'Alice', true),
    (2, 'Bob', false);

SELECT row_to_json(u)
FROM json_gap.users u
ORDER BY id;

SELECT to_jsonb(u)
FROM json_gap.users u
ORDER BY id;


-- ============================================================
-- 036. LARGE DOCUMENT
-- ============================================================

WITH documents AS (
    SELECT jsonb_build_object(
        'id', i,
        'customer',
        jsonb_build_object(
            'id', 100000 + i,
            'profile',
            jsonb_build_object(
                'name', 'Customer ' || i,
                'metadata',
                jsonb_build_object(
                    'tier', 'enterprise',
                    'region', 'IN'
                )
            )
        ),
        'tags',
        jsonb_build_array(
            'sql',
            'json',
            'database',
            'enterprise'
        )
    ) AS payload
    FROM generate_series(1,1000) i
)
SELECT count(*)
FROM documents
WHERE payload @> '{"customer":{"metadata":{"tier":"enterprise"}}}'::jsonb;


-- ============================================================
-- 037. LARGE ARRAY
-- ============================================================

SELECT jsonb_array_length(
    (
        SELECT jsonb_agg(i)
        FROM generate_series(1,10000) i
    )
);

SELECT count(*)
FROM jsonb_array_elements(
    (
        SELECT jsonb_agg(i)
        FROM generate_series(1,10000) i
    )
);


-- ============================================================
-- 038. DEEP DOCUMENT
-- ============================================================

SELECT
    jsonb_build_object(
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
                                jsonb_build_object(
                                    'h',
                                    jsonb_build_object(
                                        'i',
                                        jsonb_build_object(
                                            'j', 123
                                        )
                                    )
                                )
                            )
                        )
                    )
                )
            )
        )
    );


-- ============================================================
-- 039. UNICODE DOCUMENT
-- ============================================================

SELECT jsonb_build_object(
    'english', 'Hello',
    'telugu', 'హైదరాబాద్',
    'japanese', '東京',
    'russian', 'Москва',
    'arabic', 'العربية',
    'emoji', '😀🚀'
);


-- ============================================================
-- 040. GENERATED COLUMN
-- ============================================================

CREATE TABLE json_gap.generated (
    id INTEGER PRIMARY KEY,
    payload JSONB,
    customer_name TEXT
        GENERATED ALWAYS AS
        (payload -> 'customer' ->> 'name')
        STORED
);

INSERT INTO json_gap.generated (id, payload)
VALUES
(
    1,
    '{"customer":{"name":"Alice"}}'
);

SELECT *
FROM json_gap.generated;


-- ============================================================
-- 041. JSONB INDEX
-- ============================================================

CREATE INDEX generated_payload_gin
ON json_gap.generated
USING GIN(payload);

EXPLAIN
SELECT *
FROM json_gap.generated
WHERE payload @> '{"customer":{"name":"Alice"}}'::jsonb;


-- ============================================================
-- 042. EXPRESSION INDEX
-- ============================================================

CREATE INDEX generated_customer_name_idx
ON json_gap.generated
(
    (payload ->> 'customer')
);

EXPLAIN
SELECT *
FROM json_gap.generated
WHERE payload ->> 'customer' = '{"name":"Alice"}';


-- ============================================================
-- 043. REALISTIC API DOCUMENT WORKLOAD
-- ============================================================

WITH api_documents AS (
    SELECT jsonb_build_object(
        'request_id', i,
        'user',
        jsonb_build_object(
            'id', 1000 + i,
            'profile',
            jsonb_build_object(
                'name', 'User ' || i,
                'preferences',
                jsonb_build_object(
                    'language', 'en',
                    'notifications', true
                )
            )
        ),
        'request',
        jsonb_build_object(
            'method', 'POST',
            'endpoint', '/api/orders',
            'headers',
            jsonb_build_object(
                'content_type', 'application/json'
            ),
            'body',
            jsonb_build_object(
                'items',
                jsonb_build_array(
                    jsonb_build_object(
                        'sku', 'SKU-001',
                        'quantity', 2
                    ),
                    jsonb_build_object(
                        'sku', 'SKU-002',
                        'quantity', 1
                    )
                )
            )
        )
    ) AS document
    FROM generate_series(1,1000) i
)
SELECT count(*)
FROM api_documents
WHERE document
    -> 'request'
    -> 'body'
    -> 'items'
    -> 0
    ->> 'sku'
    = 'SKU-001';


-- ============================================================
-- 044. REALISTIC NESTED LATERAL WORKLOAD
-- ============================================================

SELECT
    d.id,
    item ->> 'sku' AS sku,
    (item ->> 'qty')::INTEGER AS qty
FROM json_gap.documents d
CROSS JOIN LATERAL
    jsonb_array_elements(
        d.payload -> 'orders'
    ) AS orders
CROSS JOIN LATERAL
    jsonb_array_elements(
        orders -> 'items'
    ) AS item
ORDER BY d.id, sku;


-- ============================================================
-- 045. FINAL DOCUMENT INTEGRATION
-- ============================================================

WITH source AS (
    SELECT
        i,
        jsonb_build_object(
            'id', i,
            'customer',
            jsonb_build_object(
                'id', 1000 + i,
                'name', 'Customer ' || i
            ),
            'active', i % 2 = 0,
            'tags',
            jsonb_build_array(
                'sql',
                'json',
                CASE
                    WHEN i % 2 = 0
                    THEN 'even'
                    ELSE 'odd'
                END
            ),
            'orders',
            jsonb_build_array(
                jsonb_build_object(
                    'id', i * 10,
                    'status', 'open',
                    'items',
                    jsonb_build_array(
                        jsonb_build_object(
                            'sku', 'SKU-001',
                            'qty', i
                        )
                    )
                )
            )
        ) AS document
    FROM generate_series(1,1000) i
)
SELECT count(*)
FROM source
WHERE document
    @> '{"customer":{"name":"Customer 500"}}'::jsonb
    OR document
    ->> 'active' = 'true';


-- ============================================================
-- 046. FINAL REGRESSION
-- ============================================================

SELECT jsonb_typeof('{}'::jsonb);
SELECT jsonb_typeof('[]'::jsonb);
SELECT jsonb_typeof('"x"'::jsonb);
SELECT jsonb_typeof('1'::jsonb);
SELECT jsonb_typeof('true'::jsonb);
SELECT jsonb_typeof('null'::jsonb);

SELECT
    '{"a":1,"b":{"c":[1,2,3]}}'::jsonb
    #>> '{b,c,1}';

SELECT
    '{"a":[1,2,3]}'::jsonb
    @> '{"a":[2]}'::jsonb;

SELECT
    jsonb_array_length(
        '{"a":[1,2,3,4,5]}'::jsonb -> 'a'
    );

SELECT
    jsonb_set(
        '{"a":{"b":1}}'::jsonb,
        '{a,b}',
        '99'::jsonb
    );

-- ============================================================
-- END
-- ============================================================