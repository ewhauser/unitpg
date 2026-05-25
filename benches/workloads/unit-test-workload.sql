\set tenant_id random(1, 1000000)

BEGIN;

INSERT INTO unit_test_workload_customers (id, tenant_id, region, name) VALUES
    (:client_id * 1000 + 1, :tenant_id, 'north', 'Ada'),
    (:client_id * 1000 + 2, :tenant_id, 'north', 'Ben'),
    (:client_id * 1000 + 3, :tenant_id, 'south', 'Cy'),
    (:client_id * 1000 + 4, :tenant_id, 'south', 'Dia'),
    (:client_id * 1000 + 5, :tenant_id, 'west', 'Eli'),
    (:client_id * 1000 + 6, :tenant_id, 'west', 'Fay');

INSERT INTO unit_test_workload_orders (id, customer_id, created_on, status, priority) VALUES
    (:client_id * 1000 + 101, :client_id * 1000 + 1, DATE '2024-01-03', 'open', 1),
    (:client_id * 1000 + 102, :client_id * 1000 + 1, DATE '2024-01-10', 'paid', 2),
    (:client_id * 1000 + 103, :client_id * 1000 + 2, DATE '2024-01-11', 'paid', 1),
    (:client_id * 1000 + 104, :client_id * 1000 + 3, DATE '2024-02-01', 'open', 3),
    (:client_id * 1000 + 105, :client_id * 1000 + 3, DATE '2024-02-13', 'paid', 2),
    (:client_id * 1000 + 106, :client_id * 1000 + 4, DATE '2024-03-05', 'cancelled', 0),
    (:client_id * 1000 + 107, :client_id * 1000 + 5, DATE '2024-03-08', 'open', 1),
    (:client_id * 1000 + 108, :client_id * 1000 + 6, DATE '2024-03-21', 'paid', 2);

INSERT INTO unit_test_workload_items (order_id, line_no, sku, quantity, unit_price) VALUES
    (:client_id * 1000 + 101, 1, 'widget', 2, 19.95),
    (:client_id * 1000 + 101, 2, 'cable', 5, 3.50),
    (:client_id * 1000 + 102, 1, 'widget', 1, 19.95),
    (:client_id * 1000 + 102, 2, 'stand', 1, 49.00),
    (:client_id * 1000 + 103, 1, 'sensor', 4, 12.25),
    (:client_id * 1000 + 104, 1, 'panel', 2, 75.00),
    (:client_id * 1000 + 104, 2, 'cable', 3, 3.50),
    (:client_id * 1000 + 105, 1, 'sensor', 1, 12.25),
    (:client_id * 1000 + 105, 2, 'stand', 2, 49.00),
    (:client_id * 1000 + 106, 1, 'panel', 1, 75.00),
    (:client_id * 1000 + 107, 1, 'widget', 3, 19.95),
    (:client_id * 1000 + 107, 2, 'sensor', 2, 12.25),
    (:client_id * 1000 + 108, 1, 'stand', 1, 49.00),
    (:client_id * 1000 + 108, 2, 'cable', 10, 3.50);

UPDATE unit_test_workload_orders
SET priority = priority + 1
WHERE customer_id BETWEEN (:client_id * 1000 + 1) AND (:client_id * 1000 + 6)
  AND status IN ('open', 'paid');

WITH order_totals AS (
    SELECT
        o.id,
        o.customer_id,
        c.region,
        o.status,
        SUM(i.quantity * i.unit_price) AS total
    FROM unit_test_workload_orders o
    JOIN unit_test_workload_customers c ON c.id = o.customer_id
    JOIN unit_test_workload_items i ON i.order_id = o.id
    WHERE c.tenant_id = :tenant_id
      AND c.id BETWEEN (:client_id * 1000 + 1) AND (:client_id * 1000 + 6)
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
FROM unit_test_workload_customers c
JOIN unit_test_workload_orders o ON o.customer_id = c.id
JOIN unit_test_workload_items i ON i.order_id = o.id
WHERE c.tenant_id = :tenant_id
  AND c.id BETWEEN (:client_id * 1000 + 1) AND (:client_id * 1000 + 6)
  AND EXISTS (
      SELECT 1
      FROM unit_test_workload_orders recent
      WHERE recent.customer_id = c.id
        AND recent.created_on >= DATE '2024-02-01'
  )
GROUP BY c.region, c.name
HAVING SUM(i.quantity * i.unit_price) > 40
ORDER BY gross DESC, c.name
LIMIT 5;

ROLLBACK;
