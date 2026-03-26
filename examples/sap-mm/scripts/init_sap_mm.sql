-- ─────────────────────────────────────────────────────────────────────────────
-- esync SAP MM (Material Management) example — PostgreSQL init script
-- ─────────────────────────────────────────────────────────────────────────────
-- Covers the core MM master-data objects:
--   material_master     → the central material record (like SAP MARA/MAKT/MARC)
--   plant_data          → plant-level views of a material (like SAP MARC)
--   storage_location    → stock quantities per plant/sloc (like SAP MARD)
--   vendor_master       → vendor header record (like SAP LFA1/LFB1)
--   purchasing_info     → material-vendor purchasing data (like SAP EINE/EINA)
--   material_document   → stock movement records (like SAP MSEG/MKPF)
-- ─────────────────────────────────────────────────────────────────────────────

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- ── 1. Material Master (MARA + MAKT rolled into one record) ───────────────────

CREATE TABLE IF NOT EXISTS material_master (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_number     TEXT NOT NULL UNIQUE,           -- SAP: MATNR
    description         TEXT NOT NULL,                  -- SAP: MAKT.MAKTX (en)
    material_type       TEXT NOT NULL,                  -- SAP: MTART (ROH, HALB, FERT, HAWA …)
    material_group      TEXT,                           -- SAP: MATKL
    base_unit           TEXT NOT NULL DEFAULT 'EA',     -- SAP: MEINS
    gross_weight        NUMERIC(15,3),
    net_weight          NUMERIC(15,3),
    weight_unit         TEXT DEFAULT 'KG',
    volume              NUMERIC(15,3),
    volume_unit         TEXT,
    ean_upc             TEXT,
    old_material_number TEXT,                           -- SAP: BISMT
    hazardous_material  TEXT,                           -- SAP: GEFAHRNUM
    created_by          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at          TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_mm_material_type  ON material_master(material_type);
CREATE INDEX IF NOT EXISTS idx_mm_material_group ON material_master(material_group);

-- ── 2. Plant Data — one row per (material, plant) ─────────────────────────────

CREATE TABLE IF NOT EXISTS plant_data (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id         UUID NOT NULL REFERENCES material_master(id),
    plant               TEXT NOT NULL,                  -- SAP: WERKS
    mrp_type            TEXT DEFAULT 'PD',              -- SAP: DISMM
    mrp_controller      TEXT,                           -- SAP: DISPO
    reorder_point       NUMERIC(15,3) DEFAULT 0,        -- SAP: MINBE
    safety_stock        NUMERIC(15,3) DEFAULT 0,        -- SAP: EISBE
    max_stock           NUMERIC(15,3),
    lot_size            TEXT DEFAULT 'EX',              -- SAP: DISLS
    planned_delivery    INTEGER DEFAULT 0,              -- SAP: PLIFZ (days)
    purchasing_group    TEXT,                           -- SAP: EKGRP
    profit_center       TEXT,
    valuation_class     TEXT,                           -- SAP: BKLAS
    standard_price      NUMERIC(15,2),                  -- SAP: STPRS
    moving_avg_price    NUMERIC(15,2),                  -- SAP: VERPR
    price_control       CHAR(1) DEFAULT 'S',            -- SAP: VPRSV  S=Standard, V=Moving Avg
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(material_id, plant)
);

CREATE INDEX IF NOT EXISTS idx_pd_plant ON plant_data(plant);

-- ── 3. Storage Location — stock on hand ───────────────────────────────────────

CREATE TABLE IF NOT EXISTS storage_location (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id         UUID NOT NULL REFERENCES material_master(id),
    plant               TEXT NOT NULL,
    sloc                TEXT NOT NULL,                  -- SAP: LGORT
    unrestricted_stock  NUMERIC(15,3) NOT NULL DEFAULT 0, -- SAP: LABST
    quality_stock       NUMERIC(15,3) NOT NULL DEFAULT 0, -- SAP: EINME
    blocked_stock       NUMERIC(15,3) NOT NULL DEFAULT 0, -- SAP: SPEME
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(material_id, plant, sloc)
);

-- ── 4. Vendor Master ──────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS vendor_master (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vendor_number       TEXT NOT NULL UNIQUE,           -- SAP: LIFNR
    name                TEXT NOT NULL,                  -- SAP: NAME1
    name2               TEXT,
    search_term         TEXT,                           -- SAP: SORTL
    street              TEXT,
    city                TEXT,
    postal_code         TEXT,
    country             CHAR(2),
    region              TEXT,
    language            CHAR(2) DEFAULT 'EN',
    phone               TEXT,
    email               TEXT,
    tax_number          TEXT,                           -- SAP: STCD1
    payment_terms       TEXT,                           -- SAP: ZTERM
    payment_method      TEXT,                           -- SAP: ZWELS
    account_group       TEXT,                           -- SAP: KTOKK
    currency            CHAR(3) DEFAULT 'USD',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at          TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_vm_account_group ON vendor_master(account_group);
CREATE INDEX IF NOT EXISTS idx_vm_country       ON vendor_master(country);

-- ── 5. Purchasing Info Record — links material ↔ vendor per plant ─────────────

CREATE TABLE IF NOT EXISTS purchasing_info (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id         UUID NOT NULL REFERENCES material_master(id),
    vendor_id           UUID NOT NULL REFERENCES vendor_master(id),
    plant               TEXT,                           -- NULL = client-wide
    purchasing_org      TEXT NOT NULL,                  -- SAP: EKORG
    net_price           NUMERIC(15,4),
    price_unit          NUMERIC(5) DEFAULT 1,
    currency            CHAR(3) DEFAULT 'USD',
    vendor_material_no  TEXT,                           -- SAP: IDNLF
    planned_delivery    INTEGER,
    minimum_qty         NUMERIC(15,3),
    reminder1_days      INTEGER,
    reminder2_days      INTEGER,
    valid_from          DATE,
    valid_to            DATE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(material_id, vendor_id, plant, purchasing_org)
);

-- ── 6. Material Document — goods movements ────────────────────────────────────

CREATE TABLE IF NOT EXISTS material_document (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_number          TEXT NOT NULL,                  -- SAP: MBLNR
    doc_year            CHAR(4) NOT NULL,               -- SAP: MJAHR
    line_item           INTEGER NOT NULL DEFAULT 1,     -- SAP: ZEILE
    material_id         UUID NOT NULL REFERENCES material_master(id),
    plant               TEXT NOT NULL,
    sloc                TEXT,
    movement_type       TEXT NOT NULL,                  -- SAP: BWART (101,261,301 …)
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

CREATE INDEX IF NOT EXISTS idx_mdoc_material    ON material_document(material_id);
CREATE INDEX IF NOT EXISTS idx_mdoc_posting     ON material_document(posting_date);
CREATE INDEX IF NOT EXISTS idx_mdoc_movement    ON material_document(movement_type);

-- ─────────────────────────────────────────────────────────────────────────────
-- CDC trigger function (reusable across all tables)
-- ─────────────────────────────────────────────────────────────────────────────

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

    PERFORM pg_notify(TG_ARGV[0], payload::TEXT);
    RETURN rec;
END;
$$ LANGUAGE plpgsql;

-- ── Attach CDC triggers ───────────────────────────────────────────────────────

DROP TRIGGER IF EXISTS material_master_cdc ON material_master;
CREATE TRIGGER material_master_cdc
AFTER INSERT OR UPDATE OR DELETE ON material_master
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_material_changes');

DROP TRIGGER IF EXISTS plant_data_cdc ON plant_data;
CREATE TRIGGER plant_data_cdc
AFTER INSERT OR UPDATE OR DELETE ON plant_data
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_plant_changes');

DROP TRIGGER IF EXISTS storage_location_cdc ON storage_location;
CREATE TRIGGER storage_location_cdc
AFTER INSERT OR UPDATE OR DELETE ON storage_location
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_stock_changes');

DROP TRIGGER IF EXISTS vendor_master_cdc ON vendor_master;
CREATE TRIGGER vendor_master_cdc
AFTER INSERT OR UPDATE OR DELETE ON vendor_master
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_vendor_changes');

DROP TRIGGER IF EXISTS purchasing_info_cdc ON purchasing_info;
CREATE TRIGGER purchasing_info_cdc
AFTER INSERT OR UPDATE OR DELETE ON purchasing_info
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_purchinfo_changes');

DROP TRIGGER IF EXISTS material_document_cdc ON material_document;
CREATE TRIGGER material_document_cdc
AFTER INSERT OR UPDATE OR DELETE ON material_document
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('mm_movedoc_changes');

-- ─────────────────────────────────────────────────────────────────────────────
-- Seed data
-- ─────────────────────────────────────────────────────────────────────────────

INSERT INTO vendor_master (vendor_number, name, search_term, street, city, postal_code, country, currency, payment_terms, account_group)
VALUES
    ('V000001', 'Acme Steel Corp',        'ACME',    '100 Industrial Blvd', 'Detroit',   '48201', 'US', 'USD', 'N030', 'LIEF'),
    ('V000002', 'GlobalParts GmbH',       'GLOBALP', 'Industriestr. 42',    'Stuttgart', '70174', 'DE', 'EUR', 'Z014', 'LIEF'),
    ('V000003', 'Pacific Plastics Ltd',   'PACPLAS', '88 Harbor View',      'Seattle',   '98101', 'US', 'USD', 'N030', 'LIEF')
ON CONFLICT DO NOTHING;

INSERT INTO material_master (material_number, description, material_type, material_group, base_unit, gross_weight, net_weight, weight_unit)
VALUES
    ('MAT-1000', 'Carbon Steel Sheet 3mm',      'ROH',  'MG-STEEL',   'EA',  25.000, 24.500, 'KG'),
    ('MAT-1001', 'Stainless Bolt M8x30',        'ROH',  'MG-FASTENER','PC',   0.020,  0.018, 'KG'),
    ('MAT-2000', 'Hydraulic Pump Assembly',     'HALB', 'MG-PUMP',    'EA',  12.500, 11.200, 'KG'),
    ('MAT-3000', 'Industrial Control Unit',     'FERT', 'MG-ELECTRO', 'EA',   5.000,  4.200, 'KG'),
    ('MAT-3001', 'Safety Valve SV-200',         'FERT', 'MG-VALVE',   'EA',   2.800,  2.600, 'KG')
ON CONFLICT DO NOTHING;

-- Plant data for plant 1000 (Hamburg)
INSERT INTO plant_data (material_id, plant, mrp_type, reorder_point, safety_stock, planned_delivery, purchasing_group, valuation_class, standard_price, price_control)
SELECT id, '1000', 'PD', 50, 20, 7, 'EK1', '3000', 85.00, 'S'
FROM material_master WHERE material_number = 'MAT-1000' ON CONFLICT DO NOTHING;

INSERT INTO plant_data (material_id, plant, mrp_type, reorder_point, safety_stock, planned_delivery, purchasing_group, valuation_class, standard_price, price_control)
SELECT id, '1000', 'PD', 200, 100, 3, 'EK1', '3001', 0.50, 'S'
FROM material_master WHERE material_number = 'MAT-1001' ON CONFLICT DO NOTHING;

INSERT INTO plant_data (material_id, plant, mrp_type, reorder_point, safety_stock, planned_delivery, purchasing_group, valuation_class, standard_price, price_control)
SELECT id, '1000', 'PD', 10, 5, 14, 'EK2', '7900', 320.00, 'S'
FROM material_master WHERE material_number = 'MAT-2000' ON CONFLICT DO NOTHING;

INSERT INTO plant_data (material_id, plant, mrp_type, reorder_point, safety_stock, planned_delivery, purchasing_group, valuation_class, standard_price, price_control)
SELECT id, '1000', 'PD', 5, 2, 30, 'EK2', '7920', 1250.00, 'S'
FROM material_master WHERE material_number = 'MAT-3000' ON CONFLICT DO NOTHING;

INSERT INTO plant_data (material_id, plant, mrp_type, reorder_point, safety_stock, planned_delivery, purchasing_group, valuation_class, standard_price, price_control)
SELECT id, '1000', 'PD', 20, 8, 5, 'EK2', '7910', 180.00, 'S'
FROM material_master WHERE material_number = 'MAT-3001' ON CONFLICT DO NOTHING;

-- Storage locations
INSERT INTO storage_location (material_id, plant, sloc, unrestricted_stock, quality_stock, blocked_stock)
SELECT id, '1000', '0001', 150, 0, 0
FROM material_master WHERE material_number = 'MAT-1000' ON CONFLICT DO NOTHING;

INSERT INTO storage_location (material_id, plant, sloc, unrestricted_stock, quality_stock, blocked_stock)
SELECT id, '1000', '0001', 800, 50, 0
FROM material_master WHERE material_number = 'MAT-1001' ON CONFLICT DO NOTHING;

INSERT INTO storage_location (material_id, plant, sloc, unrestricted_stock, quality_stock, blocked_stock)
SELECT id, '1000', '0002', 22, 3, 0
FROM material_master WHERE material_number = 'MAT-2000' ON CONFLICT DO NOTHING;

INSERT INTO storage_location (material_id, plant, sloc, unrestricted_stock, quality_stock, blocked_stock)
SELECT id, '1000', '0002', 8, 0, 1
FROM material_master WHERE material_number = 'MAT-3000' ON CONFLICT DO NOTHING;

-- Purchasing info records
INSERT INTO purchasing_info (material_id, vendor_id, plant, purchasing_org, net_price, currency, planned_delivery, minimum_qty, valid_from, valid_to)
SELECT mm.id, vm.id, '1000', 'EU01', 78.50, 'USD', 7, 10, '2025-01-01', '2025-12-31'
FROM material_master mm, vendor_master vm
WHERE mm.material_number = 'MAT-1000' AND vm.vendor_number = 'V000001'
ON CONFLICT DO NOTHING;

INSERT INTO purchasing_info (material_id, vendor_id, plant, purchasing_org, net_price, currency, planned_delivery, minimum_qty, valid_from, valid_to)
SELECT mm.id, vm.id, '1000', 'EU01', 79.90, 'EUR', 10, 5, '2025-01-01', '2025-12-31'
FROM material_master mm, vendor_master vm
WHERE mm.material_number = 'MAT-1000' AND vm.vendor_number = 'V000002'
ON CONFLICT DO NOTHING;

INSERT INTO purchasing_info (material_id, vendor_id, plant, purchasing_org, net_price, currency, planned_delivery, minimum_qty, valid_from, valid_to)
SELECT mm.id, vm.id, '1000', 'EU01', 0.42, 'USD', 3, 500, '2025-01-01', '2025-12-31'
FROM material_master mm, vendor_master vm
WHERE mm.material_number = 'MAT-1001' AND vm.vendor_number = 'V000001'
ON CONFLICT DO NOTHING;

-- Sample goods receipt documents
INSERT INTO material_document (doc_number, doc_year, line_item, material_id, plant, sloc, movement_type, quantity, unit, posting_date, reference, vendor_id, purchase_order)
SELECT '5000000001', '2025', 1, id, '1000', '0001', '101', 100, 'EA', '2025-06-01', 'PO4500001001', (SELECT id FROM vendor_master WHERE vendor_number='V000001'), '4500001001'
FROM material_master WHERE material_number = 'MAT-1000' ON CONFLICT DO NOTHING;

INSERT INTO material_document (doc_number, doc_year, line_item, material_id, plant, sloc, movement_type, quantity, unit, posting_date, reference)
SELECT '5000000002', '2025', 1, id, '1000', '0002', '261', 5, 'EA', '2025-06-15', 'ORDER1000001'
FROM material_master WHERE material_number = 'MAT-2000' ON CONFLICT DO NOTHING;
