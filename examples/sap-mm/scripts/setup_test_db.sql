-- ─────────────────────────────────────────────────────────────────────────────
-- esync SAP MM — integration test database setup
-- Run once: psql -U esync -d esync_mm_test -f examples/sap-mm/scripts/setup_test_db.sql
-- ─────────────────────────────────────────────────────────────────────────────

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- ── Drop in dependency order ──────────────────────────────────────────────────

DROP TABLE IF EXISTS material_document  CASCADE;
DROP TABLE IF EXISTS purchasing_info    CASCADE;
DROP TABLE IF EXISTS storage_location   CASCADE;
DROP TABLE IF EXISTS plant_data         CASCADE;
DROP TABLE IF EXISTS material_master    CASCADE;
DROP TABLE IF EXISTS vendor_master      CASCADE;

-- ── Tables ────────────────────────────────────────────────────────────────────

CREATE TABLE material_master (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_number     TEXT NOT NULL UNIQUE,
    description         TEXT NOT NULL,
    material_type       TEXT NOT NULL,
    material_group      TEXT,
    base_unit           TEXT NOT NULL DEFAULT 'EA',
    gross_weight        NUMERIC(15,3),
    net_weight          NUMERIC(15,3),
    weight_unit         TEXT DEFAULT 'KG',
    volume              NUMERIC(15,3),
    volume_unit         TEXT,
    ean_upc             TEXT,
    hazardous_material  TEXT,
    created_by          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at          TIMESTAMPTZ
);

CREATE TABLE plant_data (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id         UUID NOT NULL REFERENCES material_master(id),
    plant               TEXT NOT NULL,
    mrp_type            TEXT DEFAULT 'PD',
    mrp_controller      TEXT,
    reorder_point       NUMERIC(15,3) DEFAULT 0,
    safety_stock        NUMERIC(15,3) DEFAULT 0,
    max_stock           NUMERIC(15,3),
    lot_size            TEXT DEFAULT 'EX',
    planned_delivery    INTEGER DEFAULT 0,
    purchasing_group    TEXT,
    profit_center       TEXT,
    valuation_class     TEXT,
    standard_price      NUMERIC(15,2),
    moving_avg_price    NUMERIC(15,2),
    price_control       CHAR(1) DEFAULT 'S',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(material_id, plant)
);

CREATE TABLE storage_location (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id         UUID NOT NULL REFERENCES material_master(id),
    plant               TEXT NOT NULL,
    sloc                TEXT NOT NULL,
    unrestricted_stock  NUMERIC(15,3) NOT NULL DEFAULT 0,
    quality_stock       NUMERIC(15,3) NOT NULL DEFAULT 0,
    blocked_stock       NUMERIC(15,3) NOT NULL DEFAULT 0,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(material_id, plant, sloc)
);

CREATE TABLE vendor_master (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vendor_number       TEXT NOT NULL UNIQUE,
    name                TEXT NOT NULL,
    name2               TEXT,
    search_term         TEXT,
    street              TEXT,
    city                TEXT,
    postal_code         TEXT,
    country             CHAR(2),
    region              TEXT,
    language            CHAR(2) DEFAULT 'EN',
    phone               TEXT,
    email               TEXT,
    tax_number          TEXT,
    payment_terms       TEXT,
    payment_method      TEXT,
    account_group       TEXT,
    currency            CHAR(3) DEFAULT 'USD',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at          TIMESTAMPTZ
);

CREATE TABLE purchasing_info (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id         UUID NOT NULL REFERENCES material_master(id),
    vendor_id           UUID NOT NULL REFERENCES vendor_master(id),
    plant               TEXT,
    purchasing_org      TEXT NOT NULL,
    net_price           NUMERIC(15,4),
    price_unit          NUMERIC(5) DEFAULT 1,
    currency            CHAR(3) DEFAULT 'USD',
    vendor_material_no  TEXT,
    planned_delivery    INTEGER,
    minimum_qty         NUMERIC(15,3),
    valid_from          DATE,
    valid_to            DATE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(material_id, vendor_id, plant, purchasing_org)
);

CREATE TABLE material_document (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_number          TEXT NOT NULL,
    doc_year            CHAR(4) NOT NULL,
    line_item           INTEGER NOT NULL DEFAULT 1,
    material_id         UUID NOT NULL REFERENCES material_master(id),
    plant               TEXT NOT NULL,
    sloc                TEXT,
    movement_type       TEXT NOT NULL,
    quantity            NUMERIC(15,3) NOT NULL,
    unit                TEXT NOT NULL,
    posting_date        DATE NOT NULL,
    document_date       DATE,
    reference           TEXT,
    vendor_id           UUID REFERENCES vendor_master(id),
    purchase_order      TEXT,
    po_line             INTEGER,
    created_by          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(doc_number, doc_year, line_item)
);

-- ── CDC trigger ───────────────────────────────────────────────────────────────

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

-- Only material_master and vendor_master have CDC in the test config
-- (the other tables don't need CDC tests — they're tested via index/GQL)
DROP TRIGGER IF EXISTS material_master_cdc ON material_master;
CREATE TRIGGER material_master_cdc
AFTER INSERT OR UPDATE OR DELETE ON material_master
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_test_material_changes');

DROP TRIGGER IF EXISTS vendor_master_cdc ON vendor_master;
CREATE TRIGGER vendor_master_cdc
AFTER INSERT OR UPDATE OR DELETE ON vendor_master
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_test_vendor_changes');

-- ── Seed procedure ────────────────────────────────────────────────────────────
-- Fixed UUIDs keep cross-table FK references stable across reseeds.

CREATE OR REPLACE PROCEDURE seed_mm_test_data() AS $$
BEGIN
    -- Clear in FK order
    DELETE FROM material_document;
    DELETE FROM purchasing_info;
    DELETE FROM storage_location;
    DELETE FROM plant_data;
    DELETE FROM material_master;
    DELETE FROM vendor_master;

    -- Vendors
    INSERT INTO vendor_master (id, vendor_number, name, search_term, city, country, currency, payment_terms, account_group)
    VALUES
        ('aa000000-0000-0000-0000-000000000001', 'V000001', 'Acme Steel Corp',      'ACME',    'Detroit',   'US', 'USD', 'N030', 'LIEF'),
        ('aa000000-0000-0000-0000-000000000002', 'V000002', 'GlobalParts GmbH',     'GLOBALP', 'Stuttgart', 'DE', 'EUR', 'Z014', 'LIEF'),
        ('aa000000-0000-0000-0000-000000000003', 'V000003', 'Pacific Plastics Ltd', 'PACPLAS', 'Seattle',   'US', 'USD', 'N030', 'LIEF');

    -- Materials
    INSERT INTO material_master (id, material_number, description, material_type, material_group, base_unit, gross_weight, net_weight, weight_unit)
    VALUES
        ('bb000000-0000-0000-0000-000000000001', 'MAT-1000', 'Carbon Steel Sheet 3mm',  'ROH',  'MG-STEEL',   'EA', 25.000, 24.500, 'KG'),
        ('bb000000-0000-0000-0000-000000000002', 'MAT-1001', 'Stainless Bolt M8x30',    'ROH',  'MG-FASTENER','PC',  0.020,  0.018, 'KG'),
        ('bb000000-0000-0000-0000-000000000003', 'MAT-2000', 'Hydraulic Pump Assembly', 'HALB', 'MG-PUMP',    'EA', 12.500, 11.200, 'KG'),
        ('bb000000-0000-0000-0000-000000000004', 'MAT-3000', 'Industrial Control Unit', 'FERT', 'MG-ELECTRO', 'EA',  5.000,  4.200, 'KG'),
        ('bb000000-0000-0000-0000-000000000005', 'MAT-INACT','Obsolete Part',           'ROH',  'MG-STEEL',   'EA',  1.000,  0.900, 'KG');

    -- Plant data
    INSERT INTO plant_data (material_id, plant, mrp_type, reorder_point, safety_stock, planned_delivery, purchasing_group, valuation_class, standard_price, price_control)
    VALUES
        ('bb000000-0000-0000-0000-000000000001', '1000', 'PD', 50,  20, 7,  'EK1', '3000', 85.00,   'S'),
        ('bb000000-0000-0000-0000-000000000002', '1000', 'PD', 200, 100, 3, 'EK1', '3001', 0.50,    'S'),
        ('bb000000-0000-0000-0000-000000000003', '1000', 'PD', 10,  5,  14, 'EK2', '7900', 320.00,  'S'),
        ('bb000000-0000-0000-0000-000000000004', '1000', 'PD', 5,   2,  30, 'EK2', '7920', 1250.00, 'S');

    -- Storage locations
    INSERT INTO storage_location (material_id, plant, sloc, unrestricted_stock, quality_stock, blocked_stock)
    VALUES
        ('bb000000-0000-0000-0000-000000000001', '1000', '0001', 150, 0,  0),
        ('bb000000-0000-0000-0000-000000000002', '1000', '0001', 800, 50, 0),
        ('bb000000-0000-0000-0000-000000000003', '1000', '0002', 22,  3,  0),
        ('bb000000-0000-0000-0000-000000000004', '1000', '0002', 8,   0,  1);

    -- Purchasing info
    INSERT INTO purchasing_info (material_id, vendor_id, plant, purchasing_org, net_price, currency, planned_delivery, minimum_qty, valid_from, valid_to)
    VALUES
        ('bb000000-0000-0000-0000-000000000001', 'aa000000-0000-0000-0000-000000000001', '1000', 'EU01', 78.50, 'USD', 7,  10,  '2025-01-01', '2025-12-31'),
        ('bb000000-0000-0000-0000-000000000001', 'aa000000-0000-0000-0000-000000000002', '1000', 'EU01', 79.90, 'EUR', 10, 5,   '2025-01-01', '2025-12-31'),
        ('bb000000-0000-0000-0000-000000000002', 'aa000000-0000-0000-0000-000000000001', '1000', 'EU01', 0.42,  'USD', 3,  500, '2025-01-01', '2025-12-31');

    -- Goods movements
    INSERT INTO material_document (doc_number, doc_year, line_item, material_id, plant, sloc, movement_type, quantity, unit, posting_date, vendor_id, purchase_order)
    VALUES
        ('5000000001', '2025', 1, 'bb000000-0000-0000-0000-000000000001', '1000', '0001', '101', 100, 'EA', '2025-06-01', 'aa000000-0000-0000-0000-000000000001', '4500001001'),
        ('5000000001', '2025', 2, 'bb000000-0000-0000-0000-000000000002', '1000', '0001', '101', 500, 'PC', '2025-06-01', 'aa000000-0000-0000-0000-000000000001', '4500001001'),
        ('5000000002', '2025', 1, 'bb000000-0000-0000-0000-000000000003', '1000', '0002', '261', 5,   'EA', '2025-06-15', NULL, NULL);
END;
$$ LANGUAGE plpgsql;

-- ── Initial seed + sanity check ───────────────────────────────────────────────

CALL seed_mm_test_data();

DO $$
BEGIN
    ASSERT (SELECT COUNT(*) FROM vendor_master)     = 3, 'Expected 3 vendors';
    ASSERT (SELECT COUNT(*) FROM material_master)   = 5, 'Expected 5 materials';
    ASSERT (SELECT COUNT(*) FROM plant_data)        = 4, 'Expected 4 plant_data rows';
    ASSERT (SELECT COUNT(*) FROM storage_location)  = 4, 'Expected 4 storage_location rows';
    ASSERT (SELECT COUNT(*) FROM purchasing_info)   = 3, 'Expected 3 purchasing_info rows';
    ASSERT (SELECT COUNT(*) FROM material_document) = 3, 'Expected 3 material_document rows';
    RAISE NOTICE 'SAP MM test DB setup complete ✓';
END;
$$;
