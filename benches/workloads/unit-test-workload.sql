\set tenant_id random(1, 1000000)

BEGIN;

CREATE TABLE unit_test_workload_customers_:client_id (
    id integer PRIMARY KEY,
    tenant_id integer NOT NULL,
    region text NOT NULL,
    name text NOT NULL
);

CREATE TABLE unit_test_workload_orders_:client_id (
    id integer PRIMARY KEY,
    customer_id integer NOT NULL,
    created_on date NOT NULL,
    status text NOT NULL
);

CREATE TABLE unit_test_workload_items_:client_id (
    order_id integer NOT NULL,
    line_no integer NOT NULL,
    sku text NOT NULL,
    quantity integer NOT NULL,
    unit_price numeric(12, 2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
);

ALTER TABLE unit_test_workload_orders_:client_id
    ADD COLUMN priority integer NOT NULL DEFAULT 0;

ALTER TABLE unit_test_workload_customers_:client_id
    ADD COLUMN active boolean NOT NULL DEFAULT true;

CREATE INDEX unit_test_workload_orders_customer_status_idx_:client_id
    ON unit_test_workload_orders_:client_id (customer_id, status);

INSERT INTO unit_test_workload_customers_:client_id (id, tenant_id, region, name) VALUES
    (1, :tenant_id, 'north', 'Ada'),
    (2, :tenant_id, 'north', 'Ben'),
    (3, :tenant_id, 'south', 'Cy'),
    (4, :tenant_id, 'south', 'Dia'),
    (5, :tenant_id, 'west', 'Eli'),
    (6, :tenant_id, 'west', 'Fay');

INSERT INTO unit_test_workload_orders_:client_id (id, customer_id, created_on, status, priority) VALUES
    (101, 1, DATE '2024-01-03', 'open', 1),
    (102, 1, DATE '2024-01-10', 'paid', 2),
    (103, 2, DATE '2024-01-11', 'paid', 1),
    (104, 3, DATE '2024-02-01', 'open', 3),
    (105, 3, DATE '2024-02-13', 'paid', 2),
    (106, 4, DATE '2024-03-05', 'cancelled', 0),
    (107, 5, DATE '2024-03-08', 'open', 1),
    (108, 6, DATE '2024-03-21', 'paid', 2);

INSERT INTO unit_test_workload_items_:client_id (order_id, line_no, sku, quantity, unit_price) VALUES
    (101, 1, 'widget', 2, 19.95),
    (101, 2, 'cable', 5, 3.50),
    (102, 1, 'widget', 1, 19.95),
    (102, 2, 'stand', 1, 49.00),
    (103, 1, 'sensor', 4, 12.25),
    (104, 1, 'panel', 2, 75.00),
    (104, 2, 'cable', 3, 3.50),
    (105, 1, 'sensor', 1, 12.25),
    (105, 2, 'stand', 2, 49.00),
    (106, 1, 'panel', 1, 75.00),
    (107, 1, 'widget', 3, 19.95),
    (107, 2, 'sensor', 2, 12.25),
    (108, 1, 'stand', 1, 49.00),
    (108, 2, 'cable', 10, 3.50);

UPDATE unit_test_workload_orders_:client_id
SET priority = priority + 1
WHERE status IN ('open', 'paid');

WITH order_totals AS (
    SELECT
        o.id,
        o.customer_id,
        c.region,
        o.status,
        SUM(i.quantity * i.unit_price) AS total
    FROM unit_test_workload_orders_:client_id o
    JOIN unit_test_workload_customers_:client_id c ON c.id = o.customer_id
    JOIN unit_test_workload_items_:client_id i ON i.order_id = o.id
    WHERE c.tenant_id = :tenant_id
    GROUP BY o.id, o.customer_id, c.region, o.status
),
regional_totals AS (
    SELECT
        region,
        COUNT(*) AS order_count,
        SUM(total) AS revenue,
        SUM(CASE WHEN status = 'open' THEN 1 ELSE 0 END) AS open_orders
    FROM order_totals
    GROUP BY region
)
SELECT
    region,
    order_count,
    revenue,
    open_orders
FROM regional_totals
WHERE revenue > 50
ORDER BY revenue DESC, region;

SELECT
    c.region,
    c.name,
    COUNT(DISTINCT o.id) AS orders,
    SUM(i.quantity * i.unit_price) AS gross
FROM unit_test_workload_customers_:client_id c
JOIN unit_test_workload_orders_:client_id o ON o.customer_id = c.id
JOIN unit_test_workload_items_:client_id i ON i.order_id = o.id
WHERE c.tenant_id = :tenant_id
  AND EXISTS (
      SELECT 1
      FROM unit_test_workload_orders_:client_id recent
      WHERE recent.customer_id = c.id
        AND recent.created_on >= DATE '2024-02-01'
  )
GROUP BY c.region, c.name
HAVING SUM(i.quantity * i.unit_price) > 40
ORDER BY gross DESC, c.name
LIMIT 5;

ROLLBACK;
