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
    DELETE FROM orders;
    DELETE FROM products;

    INSERT INTO products (id, name, description, price, stock, active, created_at) VALUES
        ('00000000-0000-0000-0000-000000000001', 'Alpha Widget',   'First widget',  9.99,  100, true,  '2024-01-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000002', 'Beta Gizmo',     'Second gizmo',  49.99, 50,  true,  '2024-02-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000003', 'Gamma Doohickey','Third item',    199.00, 10, true,  '2024-03-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000004', 'Delta Thing',    'Inactive item', 1.00,  0,   false, '2024-04-01T00:00:00Z'),
        ('00000000-0000-0000-0000-000000000005', 'Epsilon Part',   'Spare part',    5.50,  200, true,  '2024-05-01T00:00:00Z');

    INSERT INTO orders (id, customer_id, status, total, placed_at, metadata) VALUES
        ('10000000-0000-0000-0000-000000000001',
         'aaaaaaaa-0000-0000-0000-000000000001',
         'completed', 59.98, '2024-01-15T10:00:00Z', '{"source":"web"}'),
        ('10000000-0000-0000-0000-000000000002',
         'aaaaaaaa-0000-0000-0000-000000000002',
         'pending',  199.00, '2024-02-20T12:00:00Z', '{"source":"mobile"}'),
        ('10000000-0000-0000-0000-000000000003',
         'aaaaaaaa-0000-0000-0000-000000000001',
         'cancelled', 9.99,  '2024-03-01T08:00:00Z', '{"source":"web","promo":"SAVE10"}');
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
