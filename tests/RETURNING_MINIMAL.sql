\set ON_ERROR_STOP on

DROP SCHEMA IF EXISTS plomid_returning_debug CASCADE;
CREATE SCHEMA plomid_returning_debug;
SET search_path TO plomid_returning_debug;

CREATE TABLE customers (
    id BIGINT PRIMARY KEY,
    credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00
);

INSERT INTO customers (id) VALUES (100);

\echo '=== UPDATE RETURNING id, credit_limit (the crash case) ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING id, credit_limit;

\echo '=== UPDATE RETURNING id ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING id;

\echo '=== UPDATE RETURNING credit_limit ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING credit_limit;

\echo '=== UPDATE RETURNING * ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING *;

\echo '=== UPDATE RETURNING qualified names ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING customers.id, customers.credit_limit;

\echo '=== UPDATE RETURNING expression ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING id, credit_limit + 50;

\echo '=== Plain UPDATE (no RETURNING) ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100;

\echo '=== UPDATE RETURNING multiple rows ==='
INSERT INTO customers (id, credit_limit) VALUES (101, 1000), (102, 2000);
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id IN (101, 102)
RETURNING id, credit_limit;

\echo '=== UPDATE RETURNING zero rows (no crash) ==='
UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 999
RETURNING id, credit_limit;

\echo '=== DELETE RETURNING id ==='
DELETE FROM customers WHERE id = 101 RETURNING id;

\echo '=== DELETE RETURNING id, credit_limit ==='
DELETE FROM customers WHERE id = 102 RETURNING id, credit_limit;

\echo '=== DELETE RETURNING zero rows ==='
DELETE FROM customers WHERE id = 999 RETURNING id, credit_limit;

\echo '=== INSERT RETURNING * (default coercion regression) ==='
INSERT INTO customers (id) VALUES (200) RETURNING *;

DROP SCHEMA plomid_returning_debug CASCADE;