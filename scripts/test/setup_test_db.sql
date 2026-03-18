-- ──────────────────────────────────────────────────────────────────────────
-- esync integration test database setup
-- Run once: psql -U esync -d esync_test -f scripts/test/setup_test_db.sql
-- ──────────────────────────────────────────────────────────────────────────

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- ── Drop & recreate to guarantee clean state ─────────────────────────────

DROP TABLE IF EXISTS orders  CASCADE;
DROP TABLE IF EXISTS products CASCADE;

-- ── Tables (mirrors production schema) ───────────────────────────────────

CREATE TABLE products (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    description TEXT,
    price       NUMERIC(10,2) NOT NULL DEFAULT 0,
    stock       INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE orders (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id UUID NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    total       NUMERIC(12,2) NOT NULL DEFAULT 0,
    placed_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at  TIMESTAMPTZ,
    metadata    JSONB
);

-- ── CDC trigger (same function as prod) ──────────────────────────────────

CREATE OR REPLACE FUNCTION esync_cdc_notify()
RETURNS TRIGGER AS $$
DECLARE payload JSON;
BEGIN
    payload := json_build_object(
        'op',  TG_OP,
        'id',  (CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END).id::TEXT,
        'row', row_to_json(CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END)
    );
    PERFORM pg_notify(TG_ARGV[0], payload::TEXT);
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS products_cdc ON products;
CREATE TRIGGER products_cdc
AFTER INSERT OR UPDATE OR DELETE ON products
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('test_products_changes');

-- ── Seed helpers ──────────────────────────────────────────────────────────

-- Call this to load a reproducible dataset for every test run.
CREATE OR REPLACE PROCEDURE seed_test_data() AS $$
BEGIN
    -- Remove any extra rows inserted by tests (keep only fixed UUIDs)
    DELETE FROM orders  WHERE id NOT IN (
        '10000000-0000-0000-0000-000000000001',
        '10000000-0000-0000-0000-000000000002',
        '10000000-0000-0000-0000-000000000003'
    );
    DELETE FROM products WHERE id NOT IN (
        '00000000-0000-0000-0000-000000000001',
        '00000000-0000-0000-0000-000000000002',
        '00000000-0000-0000-0000-000000000003',
        '00000000-0000-0000-0000-000000000004',
        '00000000-0000-0000-0000-000000000005'
    );

    -- Upsert fixed rows so concurrent calls never hit PK conflicts
    INSERT INTO products (id, name, description, price, stock, active, created_at, updated_at) VALUES
        ('00000000-0000-0000-0000-000000000001', 'Alpha Widget',    'First widget',  9.99,  100, true,  '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000002', 'Beta Gizmo',      'Second gizmo',  49.99, 50,  true,  '2024-02-01T00:00:00Z', '2024-02-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000003', 'Gamma Doohickey', 'Third item',    199.00,10,  true,  '2024-03-01T00:00:00Z', '2024-03-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000004', 'Delta Thing',     'Inactive item', 1.00,  0,   false, '2024-04-01T00:00:00Z', '2024-04-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000005', 'Epsilon Part',    'Spare part',    5.50,  200, true,  '2024-05-01T00:00:00Z', '2024-05-01T00:00:00Z')
    ON CONFLICT (id) DO UPDATE SET
        name        = EXCLUDED.name,
        description = EXCLUDED.description,
        price       = EXCLUDED.price,
        stock       = EXCLUDED.stock,
        active      = EXCLUDED.active,
        updated_at  = EXCLUDED.updated_at;

    INSERT INTO orders (id, customer_id, status, total, placed_at, deleted_at, metadata) VALUES
        ('10000000-0000-0000-0000-000000000001',
         'aaaaaaaa-0000-0000-0000-000000000001',
         'completed', 59.98, '2024-01-15T10:00:00Z', NULL, '{"source":"web"}'),
        ('10000000-0000-0000-0000-000000000002',
         'aaaaaaaa-0000-0000-0000-000000000002',
         'pending',  199.00, '2024-02-20T12:00:00Z', NULL, '{"source":"mobile"}'),
        ('10000000-0000-0000-0000-000000000003',
         'aaaaaaaa-0000-0000-0000-000000000001',
         'cancelled', 9.99,  '2024-03-01T08:00:00Z', NULL, '{"source":"web","promo":"SAVE10"}')
    ON CONFLICT (id) DO UPDATE SET
        status     = EXCLUDED.status,
        total      = EXCLUDED.total,
        deleted_at = NULL,
        metadata   = EXCLUDED.metadata;

    -- Reset customer data
    DELETE FROM customer_tags;
    DELETE FROM customers WHERE id NOT IN (
        'cccccccc-0000-0000-0000-000000000001',
        'cccccccc-0000-0000-0000-000000000002'
    );
    INSERT INTO customers (id, name, email) VALUES
        ('cccccccc-0000-0000-0000-000000000001', 'Alice', 'alice@example.com'),
        ('cccccccc-0000-0000-0000-000000000002', 'Bob',   'bob@example.com')
    ON CONFLICT (id) DO UPDATE SET
        name  = EXCLUDED.name,
        email = EXCLUDED.email;

    -- Reset tag data
    DELETE FROM tags WHERE id NOT IN (
        'eeeeeeee-0000-0000-0000-000000000001',
        'eeeeeeee-0000-0000-0000-000000000002'
    );
    INSERT INTO tags (id, label) VALUES
        ('eeeeeeee-0000-0000-0000-000000000001', 'vip'),
        ('eeeeeeee-0000-0000-0000-000000000002', 'wholesale')
    ON CONFLICT (id) DO UPDATE SET label = EXCLUDED.label;

    INSERT INTO customer_tags (customer_id, tag_id) VALUES
        ('cccccccc-0000-0000-0000-000000000001', 'eeeeeeee-0000-0000-0000-000000000001'),
        ('cccccccc-0000-0000-0000-000000000001', 'eeeeeeee-0000-0000-0000-000000000002')
    ON CONFLICT DO NOTHING;

    -- Link orders to Alice
    UPDATE orders SET customer_id = 'cccccccc-0000-0000-0000-000000000001'
    WHERE id IN (
        '10000000-0000-0000-0000-000000000001',
        '10000000-0000-0000-0000-000000000002'
    );
END;
$$ LANGUAGE plpgsql;

-- Run seed immediately so the DB is ready
CALL seed_test_data();

-- ── Verify ────────────────────────────────────────────────────────────────
DO $$
BEGIN
    ASSERT (SELECT COUNT(*) FROM products) = 5, 'Expected 5 products';
    ASSERT (SELECT COUNT(*) FROM orders)   = 3, 'Expected 3 orders';
    RAISE NOTICE 'Test DB setup complete ✓';
END;
$$;

-- ── Relation support tables ───────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS customers (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT NOT NULL,
    email      TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS tags (
    id    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    label TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS customer_tags (
    customer_id UUID NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    tag_id      UUID NOT NULL REFERENCES tags(id)      ON DELETE CASCADE,
    PRIMARY KEY (customer_id, tag_id)
);

-- Seed relation data
INSERT INTO customers (id, name, email) VALUES
    ('cccccccc-0000-0000-0000-000000000001', 'Alice', 'alice@example.com'),
    ('cccccccc-0000-0000-0000-000000000002', 'Bob',   'bob@example.com')
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, email = EXCLUDED.email;

INSERT INTO tags (id, label) VALUES
    ('eeeeeeee-0000-0000-0000-000000000001', 'vip'),
    ('eeeeeeee-0000-0000-0000-000000000002', 'wholesale')
ON CONFLICT (id) DO UPDATE SET label = EXCLUDED.label;

INSERT INTO customer_tags (customer_id, tag_id) VALUES
    ('cccccccc-0000-0000-0000-000000000001', 'eeeeeeee-0000-0000-0000-000000000001'),
    ('cccccccc-0000-0000-0000-000000000001', 'eeeeeeee-0000-0000-0000-000000000002')
ON CONFLICT DO NOTHING;

-- Link existing orders to Alice
UPDATE orders SET customer_id = 'cccccccc-0000-0000-0000-000000000001'
WHERE id IN (
    '10000000-0000-0000-0000-000000000001',
    '10000000-0000-0000-0000-000000000002'
);
