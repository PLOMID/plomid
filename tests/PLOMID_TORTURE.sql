-- \set ON_ERROR_STOP on

\echo '======================================================================'
\echo ' PLOMID POSTGRESQL CORE TORTURE TEST'
\echo '======================================================================'

DROP SCHEMA IF EXISTS plomid_torture CASCADE;
CREATE SCHEMA plomid_torture;

SET search_path TO plomid_torture, public;

-- ======================================================================
-- 1. BASIC EXPRESSIONS
-- ======================================================================

\echo ''
\echo '1. BASIC EXPRESSIONS'

SELECT 1;
SELECT 1 + 2;
SELECT 10 * 20;
SELECT 100 / 4;
SELECT 10 % 3;
SELECT 2 ^ 8;
SELECT 5 < 10;
SELECT 5 <= 5;
SELECT 10 > 5;
SELECT 10 <> 5;
SELECT TRUE AND TRUE;
SELECT TRUE OR FALSE;
SELECT NOT FALSE;

-- ======================================================================
-- 2. NULL
-- ======================================================================

\echo ''
\echo '2. NULL HANDLING'

SELECT NULL;
SELECT NULL IS NULL;
SELECT NULL IS NOT NULL;
SELECT COALESCE(NULL, 123);
SELECT NULLIF(10, 10);
SELECT NULLIF(10, 20);

-- ======================================================================
-- 3. CASTS
-- ======================================================================

\echo ''
\echo '3. CASTS'

SELECT CAST('123' AS INTEGER);
SELECT '123'::INTEGER;

SELECT CAST('123.45' AS NUMERIC(10,2));
SELECT '123.45'::NUMERIC(10,2);

SELECT CAST('true' AS BOOLEAN);
SELECT 'true'::BOOLEAN;

SELECT CAST('2026-09-10' AS DATE);
SELECT '2026-09-10'::DATE;

SELECT CAST(123 AS TEXT);
SELECT 123::TEXT;

SELECT CAST(123.45 AS INTEGER);

-- ======================================================================
-- 4. TABLES
-- ======================================================================

\echo ''
\echo '4. CREATE TABLES'

CREATE TABLE customers (
    id BIGINT PRIMARY KEY,
    first_name VARCHAR(100) NOT NULL,
    last_name VARCHAR(100) NOT NULL,
    email VARCHAR(255) NOT NULL UNIQUE,
    country VARCHAR(100) NOT NULL,
    status VARCHAR(30) NOT NULL DEFAULT 'active',
    credit_limit NUMERIC(12,2) NOT NULL DEFAULT 1000.00,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE products (
    id BIGINT PRIMARY KEY,
    sku VARCHAR(50) NOT NULL UNIQUE,
    name VARCHAR(200) NOT NULL,
    category VARCHAR(100) NOT NULL,
    price NUMERIC(12,2) NOT NULL,
    stock INTEGER NOT NULL DEFAULT 0,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    metadata JSONB
);

CREATE TABLE orders (
    id BIGINT PRIMARY KEY,
    customer_id BIGINT NOT NULL,
    status VARCHAR(30) NOT NULL,
    total_amount NUMERIC(12,2) NOT NULL,
    order_date TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT orders_customer_fk
        FOREIGN KEY (customer_id)
        REFERENCES customers(id)
);

CREATE TABLE order_items (
    id BIGINT PRIMARY KEY,
    order_id BIGINT NOT NULL,
    product_id BIGINT NOT NULL,
    quantity INTEGER NOT NULL,
    unit_price NUMERIC(12,2) NOT NULL,

    CONSTRAINT items_order_fk
        FOREIGN KEY (order_id)
        REFERENCES orders(id),

    CONSTRAINT items_product_fk
        FOREIGN KEY (product_id)
        REFERENCES products(id)
);

-- ======================================================================
-- 5. INSERT
-- ======================================================================

\echo ''
\echo '5. INSERT'

INSERT INTO customers
(id, first_name, last_name, email, country, status, credit_limit)
VALUES
(1, 'Alice', 'Smith', 'alice@test.com', 'India', 'active', 5000),
(2, 'Bob', 'Jones', 'bob@test.com', 'USA', 'active', 10000),
(3, 'Charlie', 'Brown', 'charlie@test.com', 'UK', 'inactive', 2000),
(4, 'David', 'Wilson', 'david@test.com', 'India', 'active', 7500),
(5, 'Emma', 'Taylor', 'emma@test.com', 'Germany', 'active', 9000);

INSERT INTO products
(id, sku, name, category, price, stock, active, metadata)
VALUES
(1, 'SKU-001', 'Laptop', 'Electronics', 1200.00, 50,
 TRUE, '{"brand":"PLOMID","ram":16}'),

(2, 'SKU-002', 'Phone', 'Electronics', 800.00, 100,
 TRUE, '{"brand":"PLOMID","storage":256}'),

(3, 'SKU-003', 'Keyboard', 'Accessories', 100.00, 200,
 TRUE, '{"brand":"PLOMID","layout":"US"}'),

(4, 'SKU-004', 'Mouse', 'Accessories', 50.00, 300,
 TRUE, '{"brand":"PLOMID","wireless":true}'),

(5, 'SKU-005', 'Desk', 'Furniture', 500.00, 25,
 TRUE, '{"material":"wood"}');

INSERT INTO orders
(id, customer_id, status, total_amount, order_date)
VALUES
(1, 1, 'paid', 1300, '2026-01-10 10:00:00'),
(2, 1, 'paid', 850,  '2026-02-15 11:00:00'),
(3, 2, 'pending', 500, '2026-03-20 12:00:00'),
(4, 4, 'shipped', 1200, '2026-04-05 13:00:00'),
(5, 5, 'paid', 100, '2026-05-10 14:00:00');

INSERT INTO order_items
(id, order_id, product_id, quantity, unit_price)
VALUES
(1, 1, 1, 1, 1200),
(2, 1, 3, 1, 100),

(3, 2, 2, 1, 800),
(4, 2, 4, 1, 50),

(5, 3, 5, 1, 500),

(6, 4, 1, 1, 1200),

(7, 5, 3, 1, 100);

-- ======================================================================
-- 6. BASIC SELECT
-- ======================================================================

\echo ''
\echo '6. SELECT'

SELECT * FROM customers;

SELECT
    id,
    first_name,
    last_name,
    email
FROM customers
WHERE status = 'active';

SELECT *
FROM customers
ORDER BY id DESC
LIMIT 3
OFFSET 1;

-- ======================================================================
-- 7. STRING FUNCTIONS
-- ======================================================================

\echo ''
\echo '7. STRING FUNCTIONS'

SELECT
    LOWER(first_name),
    UPPER(last_name),
    LENGTH(first_name),
    CONCAT(first_name, ' ', last_name),
    CONCAT_WS('-', first_name, last_name),
    LEFT(first_name, 2),
    RIGHT(last_name, 2),
    SUBSTRING(first_name FROM 1 FOR 3),
    TRIM('  hello  '),
    LTRIM('  hello'),
    RTRIM('hello  '),
    REPLACE('hello world', 'world', 'PLOMID'),
    POSITION('a' IN first_name)
FROM customers;

-- ======================================================================
-- 8. NUMERIC FUNCTIONS
-- ======================================================================

\echo ''
\echo '8. NUMERIC FUNCTIONS'

SELECT
    ABS(-10),
    CEIL(10.2),
    CEILING(10.2),
    FLOOR(10.8),
    ROUND(123.456, 2),
    POWER(2, 10),
    SQRT(144),
    MOD(10, 3),
    GREATEST(10, 20, 5),
    LEAST(10, 20, 5);

-- ======================================================================
-- 9. DATE / TIME
-- ======================================================================

\echo ''
\echo '9. DATE / TIME'

SELECT CURRENT_TIMESTAMP;
SELECT CURRENT_DATE;
SELECT CURRENT_TIME;

SELECT
    CURRENT_TIMESTAMP,
    CURRENT_TIMESTAMP,
    NOW();

SELECT DATE '2026-09-10';
SELECT TIMESTAMP '2026-09-10 12:30:00';

SELECT
    DATE_TRUNC('month', order_date),
    COUNT(*)
FROM orders
GROUP BY DATE_TRUNC('month', order_date)
ORDER BY 1;

SELECT
    order_date + INTERVAL '1 day',
    order_date - INTERVAL '1 hour'
FROM orders;

SELECT AGE(
    TIMESTAMP '2026-09-10',
    TIMESTAMP '2025-09-10'
);

-- ======================================================================
-- 10. CASE
-- ======================================================================

\echo ''
\echo '10. CASE'

SELECT
    id,
    total_amount,
    CASE
        WHEN total_amount >= 1000 THEN 'large'
        WHEN total_amount >= 500 THEN 'medium'
        ELSE 'small'
    END AS size
FROM orders;

-- ======================================================================
-- 11. AGGREGATES
-- ======================================================================

\echo ''
\echo '11. AGGREGATES'

SELECT COUNT(*) FROM customers;
SELECT COUNT(id) FROM customers;
SELECT SUM(total_amount) FROM orders;
SELECT AVG(total_amount) FROM orders;
SELECT MIN(total_amount) FROM orders;
SELECT MAX(total_amount) FROM orders;

SELECT
    status,
    COUNT(*),
    SUM(total_amount),
    AVG(total_amount)
FROM orders
GROUP BY status;

-- ======================================================================
-- 12. FILTER
-- ======================================================================

\echo ''
\echo '12. FILTER'

SELECT
    COUNT(*) FILTER (WHERE status = 'paid'),
    COUNT(*) FILTER (WHERE status = 'pending'),
    COUNT(*) FILTER (WHERE status = 'shipped')
FROM orders;

-- ======================================================================
-- 13. HAVING
-- ======================================================================

\echo ''
\echo '13. HAVING'

SELECT
    customer_id,
    COUNT(*) AS order_count
FROM orders
GROUP BY customer_id
HAVING COUNT(*) >= 1;

-- ======================================================================
-- 14. INNER JOIN
-- ======================================================================

\echo ''
\echo '14. INNER JOIN'

SELECT
    c.id,
    c.email,
    o.id AS order_id,
    o.total_amount
FROM customers c
JOIN orders o
    ON o.customer_id = c.id
ORDER BY c.id;

-- ======================================================================
-- 15. LEFT JOIN
-- ======================================================================

\echo ''
\echo '15. LEFT JOIN'

SELECT
    c.id,
    c.email,
    COUNT(o.id)
FROM customers c
LEFT JOIN orders o
    ON o.customer_id = c.id
GROUP BY c.id, c.email
ORDER BY c.id;

-- ======================================================================
-- 16. MULTI JOIN
-- ======================================================================

\echo ''
\echo '16. MULTI TABLE JOIN'

SELECT
    o.id AS order_id,
    c.email,
    p.name,
    oi.quantity,
    oi.unit_price
FROM orders o
JOIN customers c
    ON c.id = o.customer_id
JOIN order_items oi
    ON oi.order_id = o.id
JOIN products p
    ON p.id = oi.product_id
ORDER BY o.id;

-- ======================================================================
-- 17. SUBQUERY
-- ======================================================================

\echo ''
\echo '17. SUBQUERY'

SELECT *
FROM customers
WHERE id IN (
    SELECT customer_id
    FROM orders
    WHERE total_amount > 500
);

-- ======================================================================
-- 18. EXISTS
-- ======================================================================

\echo ''
\echo '18. EXISTS'

SELECT c.*
FROM customers c
WHERE EXISTS (
    SELECT 1
    FROM orders o
    WHERE o.customer_id = c.id
);

-- ======================================================================
-- 19. ANY / ALL
-- ======================================================================

\echo ''
\echo '19. ANY / ALL'

SELECT *
FROM products
WHERE price > ANY (
    SELECT total_amount
    FROM orders
);

SELECT *
FROM products
WHERE price < ALL (
    SELECT total_amount
    FROM orders
);

-- ======================================================================
-- 20. DISTINCT
-- ======================================================================

\echo ''
\echo '20. DISTINCT'

SELECT DISTINCT country
FROM customers
ORDER BY country;

-- ======================================================================
-- 21. DISTINCT ON
-- ======================================================================

\echo ''
\echo '21. DISTINCT ON'

SELECT DISTINCT ON (customer_id)
    customer_id,
    id,
    total_amount
FROM orders
ORDER BY customer_id, total_amount DESC;

-- ======================================================================
-- 22. UNION
-- ======================================================================

\echo ''
\echo '22. UNION'

SELECT country FROM customers
UNION
SELECT category FROM products;

-- ======================================================================
-- 23. UNION ALL
-- ======================================================================

\echo ''
\echo '23. UNION ALL'

SELECT country FROM customers
UNION ALL
SELECT category FROM products;

-- ======================================================================
-- 24. INTERSECT
-- ======================================================================

\echo ''
\echo '24. INTERSECT'

SELECT country FROM customers
INTERSECT
SELECT shipping_country
FROM (
    SELECT
        c.country AS shipping_country
    FROM customers c
) x;

-- ======================================================================
-- 25. EXCEPT
-- ======================================================================

\echo ''
\echo '25. EXCEPT'

SELECT country FROM customers
EXCEPT
SELECT category FROM products;

-- ======================================================================
-- 26. CTE
-- ======================================================================

\echo ''
\echo '26. CTE'

WITH customer_totals AS (
    SELECT
        customer_id,
        SUM(total_amount) AS total
    FROM orders
    GROUP BY customer_id
)
SELECT *
FROM customer_totals
ORDER BY total DESC;

-- ======================================================================
-- 27. MULTIPLE CTE
-- ======================================================================

\echo ''
\echo '27. MULTIPLE CTE'

WITH
order_totals AS (
    SELECT customer_id, SUM(total_amount) AS total
    FROM orders
    GROUP BY customer_id
),
customer_info AS (
    SELECT id, email
    FROM customers
)
SELECT
    ci.email,
    ot.total
FROM customer_info ci
JOIN order_totals ot
    ON ot.customer_id = ci.id;

-- ======================================================================
-- 28. RECURSIVE CTE
-- ======================================================================

\echo ''
\echo '28. RECURSIVE CTE'

WITH RECURSIVE numbers AS (
    SELECT 1 AS n

    UNION ALL

    SELECT n + 1
    FROM numbers
    WHERE n < 10
)
SELECT *
FROM numbers;

-- ======================================================================
-- 29. WINDOW FUNCTIONS
-- ======================================================================

\echo ''
\echo '29. WINDOW FUNCTIONS'

SELECT
    id,
    customer_id,
    total_amount,

    ROW_NUMBER() OVER (
        ORDER BY total_amount DESC
    ) AS row_number,

    RANK() OVER (
        ORDER BY total_amount DESC
    ) AS rank_number,

    DENSE_RANK() OVER (
        ORDER BY total_amount DESC
    ) AS dense_rank,

    SUM(total_amount) OVER (
        PARTITION BY customer_id
    ) AS customer_total,

    AVG(total_amount) OVER (
        PARTITION BY customer_id
    ) AS customer_average

FROM orders
ORDER BY id;

-- ======================================================================
-- 30. LAG / LEAD
-- ======================================================================

\echo ''
\echo '30. LAG / LEAD'

SELECT
    id,
    total_amount,

    LAG(total_amount) OVER (
        ORDER BY id
    ) AS previous_amount,

    LEAD(total_amount) OVER (
        ORDER BY id
    ) AS next_amount

FROM orders
ORDER BY id;

-- ======================================================================
-- 31. FIRST_VALUE / LAST_VALUE
-- ======================================================================

\echo ''
\echo '31. FIRST_VALUE / LAST_VALUE'

SELECT
    id,
    total_amount,

    FIRST_VALUE(total_amount) OVER (
        ORDER BY id
    ) AS first_amount,

    LAST_VALUE(total_amount) OVER (
        ORDER BY id
        ROWS BETWEEN UNBOUNDED PRECEDING
        AND UNBOUNDED FOLLOWING
    ) AS last_amount

FROM orders;

-- ======================================================================
-- 32. JSONB
-- ======================================================================

\echo ''
\echo '32. JSONB'

SELECT
    id,
    metadata,
    metadata -> 'brand' AS brand,
    metadata ->> 'brand' AS brand_text
FROM products;

SELECT *
FROM products
WHERE metadata @> '{"brand":"PLOMID"}';

SELECT
    metadata ->> 'ram'
FROM products
WHERE id = 1;

-- ======================================================================
-- 33. JSONB BUILD
-- ======================================================================

\echo ''
\echo '33. JSONB FUNCTIONS'

SELECT jsonb_build_object(
    'name', 'PLOMID',
    'version', 1,
    'active', TRUE
);

SELECT jsonb_build_array(
    1,
    2,
    3,
    'hello'
);

-- ======================================================================
-- 34. ARRAYS
-- ======================================================================

\echo ''
\echo '34. ARRAYS'

CREATE TABLE array_test (
    id INTEGER PRIMARY KEY,
    tags TEXT[]
);

INSERT INTO array_test VALUES
(1, ARRAY['rust', 'postgres', 'sql']),
(2, ARRAY['database', 'testing']),
(3, ARRAY['plomid']);

SELECT * FROM array_test;

SELECT
    id,
    tags[1],
    array_length(tags, 1)
FROM array_test;

SELECT *
FROM array_test
WHERE 'postgres' = ANY(tags);

-- ======================================================================
-- 35. RETURNING
-- ======================================================================

\echo ''
\echo '35. RETURNING'

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    100,
    'Returning',
    'Test',
    'returning@test.com',
    'India'
)
RETURNING *;

UPDATE customers
SET credit_limit = credit_limit + 100
WHERE id = 100
RETURNING id, credit_limit;

DELETE FROM customers
WHERE id = 100
RETURNING id, email;

-- ======================================================================
-- 36. ON CONFLICT
-- ======================================================================

\echo ''
\echo '36. ON CONFLICT'

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    1,
    'Alice',
    'Updated',
    'alice@test.com',
    'India'
)
ON CONFLICT (id)
DO UPDATE
SET last_name = EXCLUDED.last_name
RETURNING *;

-- ======================================================================
-- 37. UPDATE FROM
-- ======================================================================

\echo ''
\echo '37. UPDATE FROM'

UPDATE products p
SET stock = p.stock + 10
FROM (
    SELECT 1 AS product_id
) x
WHERE p.id = x.product_id
RETURNING p.id, p.stock;

-- ======================================================================
-- 38. DELETE USING
-- ======================================================================

\echo ''
\echo '38. DELETE USING'

CREATE TEMP TABLE delete_test (
    id INTEGER PRIMARY KEY
);

INSERT INTO delete_test VALUES
(1),
(2),
(3);

CREATE TEMP TABLE delete_filter (
    id INTEGER PRIMARY KEY
);

INSERT INTO delete_filter VALUES
(2);

DELETE FROM delete_test d
USING delete_filter f
WHERE d.id = f.id
RETURNING d.*;

-- ======================================================================
-- 39. UPSERT
-- ======================================================================

\echo ''
\echo '39. UPSERT'

INSERT INTO products
(
    id,
    sku,
    name,
    category,
    price,
    stock,
    active
)
VALUES
(
    1,
    'SKU-001',
    'Laptop Updated',
    'Electronics',
    1250,
    75,
    TRUE
)
ON CONFLICT (id)
DO UPDATE
SET
    name = EXCLUDED.name,
    price = EXCLUDED.price,
    stock = EXCLUDED.stock
RETURNING *;

-- ======================================================================
-- 40. TRANSACTION
-- ======================================================================

\echo ''
\echo '40. TRANSACTION'

BEGIN;

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    200,
    'Transaction',
    'Commit',
    'commit@test.com',
    'India'
);

COMMIT;

-- ======================================================================
-- 41. ROLLBACK
-- ======================================================================

\echo ''
\echo '41. ROLLBACK'

BEGIN;

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    201,
    'Transaction',
    'Rollback',
    'rollback@test.com',
    'India'
);

ROLLBACK;

SELECT COUNT(*)
FROM customers
WHERE id = 201;

-- ======================================================================
-- 42. SAVEPOINT
-- ======================================================================

\echo ''
\echo '42. SAVEPOINT'

BEGIN;

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    202,
    'Savepoint',
    'Test',
    'savepoint@test.com',
    'India'
);

SAVEPOINT test_savepoint;

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    203,
    'Savepoint',
    'Rollback',
    'savepoint2@test.com',
    'USA'
);

ROLLBACK TO SAVEPOINT test_savepoint;

COMMIT;

SELECT id, email
FROM customers
WHERE id IN (202, 203)
ORDER BY id;

-- ======================================================================
-- 43. INDEXES
-- ======================================================================

\echo ''
\echo '43. INDEXES'

CREATE INDEX customers_country_idx
ON customers(country);

CREATE INDEX products_category_idx
ON products(category);

CREATE INDEX orders_customer_idx
ON orders(customer_id);

CREATE INDEX orders_date_idx
ON orders(order_date);

-- ======================================================================
-- 44. EXPLAIN
-- ======================================================================

\echo ''
\echo '44. EXPLAIN'

EXPLAIN
SELECT *
FROM customers
WHERE country = 'India';

EXPLAIN
SELECT
    c.email,
    SUM(o.total_amount)
FROM customers c
JOIN orders o
    ON o.customer_id = c.id
GROUP BY c.email;

-- ======================================================================
-- 45. VIEW
-- ======================================================================

\echo ''
\echo '45. VIEW'

CREATE VIEW customer_summary AS
SELECT
    c.id,
    c.email,
    COUNT(o.id) AS order_count,
    COALESCE(SUM(o.total_amount), 0) AS total_spent
FROM customers c
LEFT JOIN orders o
    ON o.customer_id = c.id
GROUP BY c.id, c.email;

SELECT *
FROM customer_summary
ORDER BY total_spent DESC;

-- ======================================================================
-- 46. INFORMATION_SCHEMA
-- ======================================================================

\echo ''
\echo '46. INFORMATION_SCHEMA'

SELECT
    table_schema,
    table_name
FROM information_schema.tables
WHERE table_schema = 'plomid_torture'
ORDER BY table_name;

SELECT
    table_name,
    column_name,
    data_type,
    is_nullable
FROM information_schema.columns
WHERE table_schema = 'plomid_torture'
ORDER BY table_name, ordinal_position;

SELECT
    constraint_name,
    table_name,
    constraint_type
FROM information_schema.table_constraints
WHERE table_schema = 'plomid_torture'
ORDER BY table_name, constraint_name;

-- ======================================================================
-- 47. PG CATALOG
-- ======================================================================

\echo ''
\echo '47. PG_CATALOG'

SELECT
    schemaname,
    tablename
FROM pg_catalog.pg_tables
WHERE schemaname = 'plomid_torture'
ORDER BY tablename;

SELECT
    indexname,
    tablename
FROM pg_catalog.pg_indexes
WHERE schemaname = 'plomid_torture'
ORDER BY tablename, indexname;

-- ======================================================================
-- 48. CONSTRAINT VIOLATION
-- ======================================================================

\echo ''
\echo '48. PRIMARY KEY VIOLATION'

\set ON_ERROR_STOP off

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    1,
    'Duplicate',
    'PK',
    'duplicate-pk@test.com',
    'India'
);

ROLLBACK;

\set ON_ERROR_STOP on

-- ======================================================================
-- 49. FOREIGN KEY VIOLATION
-- ======================================================================

\echo ''
\echo '49. FOREIGN KEY VIOLATION'

\set ON_ERROR_STOP off

INSERT INTO orders
(
    id,
    customer_id,
    status,
    total_amount
)
VALUES
(
    999,
    999999,
    'pending',
    100
);

ROLLBACK;

\set ON_ERROR_STOP on

-- ======================================================================
-- 50. UNIQUE VIOLATION
-- ======================================================================

\echo ''
\echo '50. UNIQUE VIOLATION'

\set ON_ERROR_STOP off

INSERT INTO customers
(
    id,
    first_name,
    last_name,
    email,
    country
)
VALUES
(
    500,
    'Duplicate',
    'Email',
    'alice@test.com',
    'India'
);

ROLLBACK;

\set ON_ERROR_STOP on

-- ======================================================================
-- 51. GROUPING SETS
-- ======================================================================

\echo ''
\echo '51. GROUPING SETS'

SELECT
    country,
    status,
    COUNT(*)
FROM customers
GROUP BY GROUPING SETS (
    (country),
    (status),
    ()
)
ORDER BY country NULLS LAST, status NULLS LAST;

-- ======================================================================
-- 52. ROLLUP
-- ======================================================================

\echo ''
\echo '52. ROLLUP'

SELECT
    country,
    status,
    COUNT(*)
FROM customers
GROUP BY ROLLUP(country, status)
ORDER BY country NULLS LAST, status NULLS LAST;

-- ======================================================================
-- 53. CUBE
-- ======================================================================

\echo ''
\echo '53. CUBE'

SELECT
    country,
    status,
    COUNT(*)
FROM customers
GROUP BY CUBE(country, status);

-- ======================================================================
-- 54. ORDER NULLS
-- ======================================================================

\echo ''
\echo '54. NULLS FIRST / LAST'

SELECT
    id,
    created_at
FROM customers
ORDER BY created_at DESC NULLS LAST;

-- ======================================================================
-- 55. BOOLEAN
-- ======================================================================

\echo ''
\echo '55. BOOLEAN'

SELECT *
FROM products
WHERE active = TRUE;

SELECT *
FROM products
WHERE active IS TRUE;

SELECT *
FROM products
WHERE active IS NOT FALSE;

-- ======================================================================
-- 56. BETWEEN / IN / LIKE
-- ======================================================================

\echo ''
\echo '56. PREDICATES'

SELECT *
FROM products
WHERE price BETWEEN 50 AND 1000;

SELECT *
FROM customers
WHERE country IN ('India', 'USA');

SELECT *
FROM customers
WHERE email LIKE '%@test.com';

SELECT *
FROM customers
WHERE first_name ILIKE 'a%';

-- ======================================================================
-- 57. CASE + NULL
-- ======================================================================

\echo ''
\echo '57. CASE + NULL'

SELECT
    id,
    CASE
        WHEN credit_limit IS NULL THEN 'missing'
        WHEN credit_limit >= 5000 THEN 'high'
        ELSE 'normal'
    END
FROM customers;

-- ======================================================================
-- 58. ARRAY FUNCTIONS
-- ======================================================================

\echo ''
\echo '58. ARRAY FUNCTIONS'

SELECT
    array_length(tags, 1),
    array_position(tags, 'postgres'),
    cardinality(tags)
FROM array_test;

-- ======================================================================
-- 59. JSON OPERATORS
-- ======================================================================

\echo ''
\echo '59. JSON OPERATORS'

SELECT
    metadata ? 'brand',
    metadata ? 'ram',
    metadata @> '{"brand":"PLOMID"}'
FROM products
WHERE id = 1;

-- ======================================================================
-- 60. FINAL COUNTS
-- ======================================================================

\echo ''
\echo '60. FINAL COUNTS'

SELECT 'customers' AS table_name, COUNT(*) FROM customers
UNION ALL
SELECT 'products', COUNT(*) FROM products
UNION ALL
SELECT 'orders', COUNT(*) FROM orders
UNION ALL
SELECT 'order_items', COUNT(*) FROM order_items
UNION ALL
SELECT 'array_test', COUNT(*) FROM array_test;

-- ======================================================================
-- 61. FINAL FK INTEGRITY
-- ======================================================================

\echo ''
\echo '61. FOREIGN KEY INTEGRITY'

SELECT COUNT(*) AS orphan_orders
FROM orders o
LEFT JOIN customers c
    ON c.id = o.customer_id
WHERE c.id IS NULL;

SELECT COUNT(*) AS orphan_items_orders
FROM order_items i
LEFT JOIN orders o
    ON o.id = i.order_id
WHERE o.id IS NULL;

SELECT COUNT(*) AS orphan_items_products
FROM order_items i
LEFT JOIN products p
    ON p.id = i.product_id
WHERE p.id IS NULL;

-- ======================================================================
-- 62. FINAL CATALOG CHECK
-- ======================================================================

\echo ''
\echo '62. FINAL CATALOG CHECK'

SELECT COUNT(*)
FROM information_schema.tables
WHERE table_schema = 'plomid_torture';

SELECT COUNT(*)
FROM information_schema.columns
WHERE table_schema = 'plomid_torture';

SELECT COUNT(*)
FROM information_schema.table_constraints
WHERE table_schema = 'plomid_torture';

-- ======================================================================
-- 63. CLEANUP
-- ======================================================================

\echo ''
\echo '63. CLEANUP'

DROP SCHEMA plomid_torture CASCADE;

-- ======================================================================
-- 64. VERIFY CLEAN
-- ======================================================================

\echo ''
\echo '64. VERIFY CLEAN'

SELECT COUNT(*)
FROM information_schema.tables
WHERE table_schema = 'plomid_torture';

\echo ''
\echo '======================================================================'
\echo ' PLOMID POSTGRESQL CORE TORTURE TEST COMPLETE'
\echo ' DATABASE CLEAN'
\echo '======================================================================'
