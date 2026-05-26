DROP TABLE IF EXISTS unit_test_workload_items;
DROP TABLE IF EXISTS unit_test_workload_orders;
DROP TABLE IF EXISTS unit_test_workload_customers;

CREATE TABLE unit_test_workload_customers (
    id integer PRIMARY KEY,
    tenant_id integer NOT NULL,
    region text NOT NULL,
    name text NOT NULL,
    active boolean NOT NULL DEFAULT true
);

CREATE TABLE unit_test_workload_orders (
    id integer PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES unit_test_workload_customers (id),
    created_on date NOT NULL,
    status text NOT NULL,
    priority integer NOT NULL DEFAULT 0
);

CREATE TABLE unit_test_workload_items (
    order_id integer NOT NULL REFERENCES unit_test_workload_orders (id),
    line_no integer NOT NULL,
    sku text NOT NULL,
    quantity integer NOT NULL,
    unit_price numeric(12, 2) NOT NULL,
    PRIMARY KEY (order_id, line_no)
);

CREATE INDEX unit_test_workload_customers_tenant_region_idx
    ON unit_test_workload_customers (tenant_id, region);

CREATE INDEX unit_test_workload_orders_customer_status_idx
    ON unit_test_workload_orders (customer_id, status);

CREATE INDEX unit_test_workload_orders_customer_created_idx
    ON unit_test_workload_orders (customer_id, created_on);
