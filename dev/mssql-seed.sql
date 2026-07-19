-- Seed data for local zsql MSSQL development. Applied by
-- scripts/mssql-dev.sh. Small, varied schema comparable in shape to
-- dev/seed.sql (the Postgres seed): a couple of tables, a view, a second
-- schema, and an empty schema -- varied so introspection has something of
-- every kind to see.

IF OBJECT_ID('dbo.recent_orders', 'V') IS NOT NULL DROP VIEW dbo.recent_orders;
IF OBJECT_ID('dbo.orders', 'U') IS NOT NULL DROP TABLE dbo.orders;
IF OBJECT_ID('dbo.users', 'U') IS NOT NULL DROP TABLE dbo.users;
IF OBJECT_ID('analytics.page_views', 'U') IS NOT NULL DROP TABLE analytics.page_views;
IF SCHEMA_ID('analytics') IS NOT NULL DROP SCHEMA analytics;
IF SCHEMA_ID('empty_ns') IS NOT NULL DROP SCHEMA empty_ns;
GO

CREATE TABLE dbo.users (
    id           int IDENTITY PRIMARY KEY,
    email        nvarchar(255) NOT NULL UNIQUE,
    display_name nvarchar(255) NULL,
    is_active    bit NOT NULL DEFAULT 1
);

CREATE TABLE dbo.orders (
    id          int IDENTITY PRIMARY KEY,
    user_id     int NOT NULL REFERENCES dbo.users (id),
    total_cents int NOT NULL,
    status      nvarchar(50) NOT NULL DEFAULT 'pending',
    placed_at   datetime2 NOT NULL DEFAULT SYSUTCDATETIME()
);
GO

-- A view, distinct from an ordinary table: it is the only seeded object
-- that exercises the `RelationKind::View` mapping.
CREATE VIEW dbo.recent_orders AS
SELECT o.id, u.email, o.total_cents, o.status, o.placed_at
FROM dbo.orders o
JOIN dbo.users u ON u.id = o.user_id;
GO

-- A second, non-dbo schema with a table, so introspection is verified to
-- walk every non-system schema, not just `dbo`.
CREATE SCHEMA analytics;
GO

CREATE TABLE analytics.page_views (
    id        int IDENTITY PRIMARY KEY,
    path      nvarchar(255) NOT NULL,
    viewed_at datetime2 NOT NULL DEFAULT SYSUTCDATETIME()
);
GO

-- A schema with zero relations, so introspection is verified to still
-- surface it (with an empty relation list) rather than silently dropping
-- schemas that hold nothing yet.
CREATE SCHEMA empty_ns;
GO

INSERT INTO dbo.users (email, display_name, is_active) VALUES
    ('ada@example.com', 'Ada', 1),
    ('lin@example.com', 'Lin', 1),
    ('rob@example.com', NULL, 1);

INSERT INTO dbo.orders (user_id, total_cents, status) VALUES
    (1, 1299, 'paid'),
    (1, 4900, 'pending'),
    (2, 250, 'refunded');

INSERT INTO analytics.page_views (path) VALUES
    ('/'),
    ('/pricing');
GO
