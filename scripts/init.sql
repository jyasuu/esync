-- ──────────────────────────────────────────────────────────────────────────
-- esync dev init script
-- ──────────────────────────────────────────────────────────────────────────

-- Extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- ── Tables ────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS products (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL,
    description TEXT,
    price       NUMERIC(10,2) NOT NULL DEFAULT 0,
    stock       INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS orders (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id UUID NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    total       NUMERIC(12,2) NOT NULL DEFAULT 0,
    placed_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at  TIMESTAMPTZ,
    metadata    JSONB
);

-- ── CDC trigger function ──────────────────────────────────────────────────
-- Sends a NOTIFY on <channel> with JSON payload:
--   { "op": "INSERT"|"UPDATE"|"DELETE", "id": "<pk>", "row": { ... } }
--
-- Usage: SELECT esync_notify_trigger('products', 'id', 'products_changes');

CREATE OR REPLACE FUNCTION esync_cdc_notify()
RETURNS TRIGGER AS $$
DECLARE
    payload JSON;
    op      TEXT;
    rec     RECORD;
BEGIN
    op  := TG_OP;
    rec := CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;

    payload := json_build_object(
        'op',  op,
        'id',  rec.id::TEXT,
        'row', row_to_json(rec)
    );

    -- Channel name is the trigger argument (TG_ARGV[0])
    PERFORM pg_notify(TG_ARGV[0], payload::TEXT);
    RETURN rec;
END;
$$ LANGUAGE plpgsql;

-- ── Attach trigger to products table ──────────────────────────────────────
DROP TRIGGER IF EXISTS products_cdc ON products;
CREATE TRIGGER products_cdc
AFTER INSERT OR UPDATE OR DELETE ON products
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('products_changes');

-- ── Seed data ─────────────────────────────────────────────────────────────
INSERT INTO products (name, description, price, stock, active) VALUES
    ('Rusty Widget',      'A fine widget made of rust',  9.99,  100, true),
    ('Electric Gizmo',    'Powers up anything',          49.99, 50,  true),
    ('Quantum Doohickey', 'Entangles your belongings',   199.00, 10, true),
    ('Plain Thingamajig', 'Just a thingamajig',          1.00,  999, false)
ON CONFLICT DO NOTHING;

INSERT INTO orders (customer_id, status, total, metadata) VALUES
    (gen_random_uuid(), 'completed', 59.98, '{"source":"web"}'),
    (gen_random_uuid(), 'pending',   199.00, '{"source":"mobile"}')
ON CONFLICT DO NOTHING;
