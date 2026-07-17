-- Seed data for local zsql development. Applied by scripts/pg-dev.sh.
-- Small, varied schema: a couple of tables, a view, a materialized view, a
-- partitioned table, a second schema, an empty schema, a same-named table in
-- two different schemas, and a few types -- deliberately varied so
-- introspection has something of every kind to see.

DROP MATERIALIZED VIEW IF EXISTS recent_orders_mv;
DROP VIEW IF EXISTS recent_orders;
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS events CASCADE;
DROP SCHEMA IF EXISTS analytics CASCADE;
DROP SCHEMA IF EXISTS empty_ns CASCADE;

CREATE TABLE users (
    id           bigserial PRIMARY KEY,
    email        text NOT NULL UNIQUE,
    display_name text,
    is_active    boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE orders (
    id          bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES users (id),
    total_cents integer NOT NULL,
    status      text NOT NULL DEFAULT 'pending',
    metadata    jsonb NOT NULL DEFAULT '{}',
    placed_at   timestamptz NOT NULL DEFAULT now()
);

CREATE VIEW recent_orders AS
SELECT o.id, u.email, o.total_cents, o.status, o.placed_at
FROM orders o
JOIN users u ON u.id = o.user_id
ORDER BY o.placed_at DESC;

-- A materialized view, distinct from the plain view above: it is backed by
-- its own on-disk storage and its own `pg_class.relkind` ('m'), so it is the
-- only seeded object that exercises the `RelationKind::MatView` mapping.
CREATE MATERIALIZED VIEW recent_orders_mv AS
SELECT o.id, u.email, o.total_cents, o.status, o.placed_at
FROM orders o
JOIN users u ON u.id = o.user_id;

-- A partitioned table, distinct from an ordinary table: the parent `events`
-- row has `pg_class.relkind = 'p'` rather than `'r'`, so it is the only
-- seeded object that exercises the partitioned-table arm of the
-- `RelationKind::Table` mapping. Its partition `events_2024` is an ordinary
-- table in its own right and is enumerated like any other table.
CREATE TABLE events (
    id          bigserial,
    occurred_at timestamptz NOT NULL,
    payload     jsonb NOT NULL DEFAULT '{}'
) PARTITION BY RANGE (occurred_at);

CREATE TABLE events_2024 PARTITION OF events
    FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');

-- A second, non-public schema with a table, so introspection is verified to
-- walk every non-system schema, not just `public`.
CREATE SCHEMA analytics;

CREATE TABLE analytics.page_views (
    id        bigserial PRIMARY KEY,
    path      text NOT NULL,
    viewed_at timestamptz NOT NULL DEFAULT now()
);

-- Same relation name as `public.users`, but a completely different column
-- set, in a different schema. This is the case that catches column
-- attribution silently regressing from (schema, relation) keying to
-- relation-name-only keying: same-named tables across schemas are common in
-- real Postgres databases, and name-only keying would mix up (or drop) one
-- of the two tables' columns.
CREATE TABLE analytics.users (
    user_id  bigserial PRIMARY KEY,
    username text NOT NULL
);

-- A schema with zero relations, so introspection is verified to still
-- surface it (with an empty relation list) rather than silently dropping
-- schemas that hold nothing yet.
CREATE SCHEMA empty_ns;

INSERT INTO users (email, display_name) VALUES
    ('ada@example.com', 'Ada'),
    ('lin@example.com', 'Lin'),
    ('rob@example.com', NULL);

INSERT INTO orders (user_id, total_cents, status, metadata) VALUES
    (1, 1299, 'paid', '{"coupon": "WELCOME"}'),
    (1, 4900, 'pending', '{}'),
    (2, 250, 'refunded', '{"reason": "duplicate"}');

INSERT INTO events (occurred_at, payload) VALUES
    ('2024-06-01T12:00:00Z', '{"kind": "signup"}');

INSERT INTO analytics.page_views (path) VALUES
    ('/'),
    ('/pricing');

INSERT INTO analytics.users (username) VALUES
    ('ada'),
    ('lin');
