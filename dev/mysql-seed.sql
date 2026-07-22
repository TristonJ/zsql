-- Seed data for local zsql MySQL/MariaDB development. Applied by
-- scripts/mysql-dev.sh against the `zsql` database it just created. Small,
-- varied schema comparable in shape to dev/seed.sql (Postgres) and
-- dev/mssql-seed.sql: a couple of tables, a view, a second database, and an
-- empty database -- varied so introspection has something of every kind to
-- see, with rows exercising every mapped type this driver supports.

DROP DATABASE IF EXISTS zsql_analytics;
DROP DATABASE IF EXISTS zsql_empty;

DROP VIEW IF EXISTS recent_orders;
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS users;

CREATE TABLE users (
    id           INT AUTO_INCREMENT PRIMARY KEY,
    email        VARCHAR(255) NOT NULL UNIQUE,
    display_name VARCHAR(255) NULL,
    is_active    BOOLEAN NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE orders (
    id          INT AUTO_INCREMENT PRIMARY KEY,
    user_id     INT NOT NULL,
    total_cents BIGINT UNSIGNED NOT NULL,
    unit_price  DECIMAL(10, 2) NOT NULL,
    status      VARCHAR(50) NOT NULL DEFAULT 'pending',
    receipt     BLOB NULL,
    metadata    JSON NULL,
    placed_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)
);

-- A view, distinct from an ordinary table: it is the only seeded object
-- that exercises the `RelationKind::View` mapping.
CREATE VIEW recent_orders AS
SELECT o.id, u.email, o.total_cents, o.status, o.placed_at
FROM orders o
JOIN users u ON u.id = o.user_id;

-- A second, non-default database with a table, so introspection is
-- verified to walk every non-system database, not just `zsql`.
CREATE DATABASE zsql_analytics;

CREATE TABLE zsql_analytics.page_views (
    id        INT AUTO_INCREMENT PRIMARY KEY,
    path      VARCHAR(255) NOT NULL,
    viewed_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- A database with zero relations, so introspection is verified to still
-- surface it (with an empty relation list) rather than silently dropping
-- databases that hold nothing yet.
CREATE DATABASE zsql_empty;

INSERT INTO users (email, display_name, is_active) VALUES
    ('ada@example.com', 'Ada', TRUE),
    ('lin@example.com', 'Lin', TRUE),
    ('rob@example.com', NULL, TRUE);

INSERT INTO orders (user_id, total_cents, unit_price, status, receipt, metadata) VALUES
    (1, 1299, 12.99, 'paid', 0x0102, JSON_OBJECT('gift', TRUE)),
    (1, 4900, 49.00, 'pending', NULL, NULL),
    (2, 250, 2.50, 'refunded', NULL, NULL);

INSERT INTO zsql_analytics.page_views (path) VALUES
    ('/'),
    ('/pricing');
