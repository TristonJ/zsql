-- Seed data for local zsql development. Applied by scripts/pg-dev.sh.
-- Small, varied schema: a couple of tables, a view, a few types.

DROP VIEW IF EXISTS recent_orders;
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS users;

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

INSERT INTO users (email, display_name) VALUES
    ('ada@example.com', 'Ada'),
    ('lin@example.com', 'Lin'),
    ('rob@example.com', NULL);

INSERT INTO orders (user_id, total_cents, status, metadata) VALUES
    (1, 1299, 'paid', '{"coupon": "WELCOME"}'),
    (1, 4900, 'pending', '{}'),
    (2, 250, 'refunded', '{"reason": "duplicate"}');
