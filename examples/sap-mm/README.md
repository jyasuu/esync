# esync — SAP MM (Material Management) Example

This directory is a **drop-in example** showing how to use [esync](../README.md) as the sync backbone for an SAP-style MM module. It requires **zero changes to the esync binary** — everything is driven by `esync-sap-mm.yaml` and the SQL init script.

---

## What's included

| File | Purpose |
|---|---|
| `esync-sap-mm.yaml` | Full entity + relation + search config for all MM objects |
| `scripts/init_sap_mm.sql` | Creates tables, CDC triggers, and seed data |
| `docker-compose.yml` | Postgres + ES + Kibana + esync services |

---

## SAP MM data model

```
material_master  ──< plant_data         (one per plant, like MARC)
                 ──< storage_location   (stock per plant/sloc, like MARD)
                 ──< purchasing_info    (price conditions per vendor, like EINA/EINE)
                 ──< material_document  (goods movements, like MSEG)

vendor_master    ──< purchasing_info
                 ──< material_document
```

### Table ↔ SAP object mapping

| PostgreSQL table | SAP equivalents | Description |
|---|---|---|
| `material_master` | MARA + MAKT | General material data + description |
| `plant_data` | MARC | MRP, valuation, purchasing group per plant |
| `storage_location` | MARD | Unrestricted / quality / blocked stock |
| `vendor_master` | LFA1 + LFB1 | Vendor general data + company data |
| `purchasing_info` | EINA + EINE | Material-vendor price conditions |
| `material_document` | MKPF + MSEG | Goods movement header + line items |

---

## Quick start

### 1. Prerequisites

- Docker & Docker Compose
- The esync binary (built from the project root: `cargo build --release`)

### 2. Start the stack

```bash
# Infrastructure only (Postgres, ES, Kibana)
docker compose up -d postgres elasticsearch kibana

# Wait for health checks, then run the initial bulk index
docker compose --profile index up esync-index

# Start the GraphQL server
docker compose up -d esync-serve

# Start the CDC watch daemon (separate terminal or background)
docker compose --profile watch up esync-watch
```

Or, if you prefer to run esync locally (without Docker for the binary):

```bash
# Infrastructure
docker compose up -d postgres elasticsearch kibana

# Bulk index
./esync index --config esync-sap-mm.yaml

# GraphQL server
./esync serve --config esync-sap-mm.yaml

# CDC watch (separate terminal)
./esync watch --config esync-sap-mm.yaml
```

### 3. Try the GraphQL API

Open **http://localhost:4000/graphql** (GraphiQL playground).

#### List materials with stock and plant data

```graphql
query {
  list_material_master(limit: 10) {
    id
    material_number
    description
    material_type
    material_group
    base_unit
    stock_levels {
      plant
      sloc
      unrestricted_stock
    }
    plant_views {
      plant
      standard_price
      reorder_point
      purchasing_group
    }
  }
}
```

#### Get a single material with full relations

```graphql
query {
  get_MaterialMaster(id: "<uuid>") {
    material_number
    description
    purchasing_infos {
      purchasing_org
      net_price
      currency
      valid_from
      valid_to
      vendor {
        vendor_number
        name
        country
      }
    }
    movements {
      doc_number
      movement_type
      quantity
      posting_date
      vendor { name }
    }
  }
}
```

#### Full-text search across materials (ES-backed)

```graphql
query {
  search_MaterialMaster(query: "hydraulic pump", limit: 5) {
    hits {
      id
      material_number
      description
      plant_views { plant standard_price }
      stock_levels { plant sloc unrestricted_stock }
    }
    total
    highlights { field snippets }
  }
}
```

#### Search vendors

```graphql
query {
  search_VendorMaster(query: "steel", limit: 5) {
    hits {
      vendor_number
      name
      country
      purchasing_infos {
        net_price
        currency
        material { material_number description }
      }
    }
    total
  }
}
```

#### List purchasing info for a vendor

```graphql
query {
  list_PurchasingInfo(limit: 20, filter: "purchasing_org = 'EU01'") {
    purchasing_org
    net_price
    currency
    valid_from
    valid_to
    material { material_number description }
    vendor    { vendor_number name }
  }
}
```

---

## CDC — live sync

The SQL init script attaches a `NOTIFY` trigger to every table. When a row changes in Postgres, esync (`watch` mode) picks it up via `LISTEN/NOTIFY` and upserts/deletes the document in Elasticsearch — typically in under a second.

| Table | Postgres channel |
|---|---|
| `material_master` | `mm_material_changes` |
| `plant_data` | `mm_plant_changes` |
| `storage_location` | `mm_stock_changes` |
| `vendor_master` | `mm_vendor_changes` |
| `purchasing_info` | `mm_purchinfo_changes` |
| `material_document` | `mm_movedoc_changes` |

Test it:

```sql
-- Connect to Postgres and update a material
UPDATE material_master
SET description = 'Carbon Steel Sheet 3mm — Grade A'
WHERE material_number = 'MAT-1000';
```

The ES document for that material updates within a second. Query ES to verify:

```bash
curl "http://localhost:9200/sap_mm_material/_doc/<id>"
```

---

## ES indices created

| Index | Entity |
|---|---|
| `sap_mm_material` | MaterialMaster |
| `sap_mm_plant_data` | PlantData |
| `sap_mm_stock` | StorageLocation |
| `sap_mm_vendor` | VendorMaster |
| `sap_mm_purchasing_info` | PurchasingInfo |
| `sap_mm_material_doc` | MaterialDocument |

---

## Extending to other SAP modules

Because esync is purely config-driven, adding another module is just YAML + SQL:

### SD (Sales & Distribution) — sketch

```yaml
entities:
  - name: SalesOrder
    table: sales_orders
    index: sap_sd_sales_order
    notify_channel: sd_order_changes
    columns:
      - { name: id,           pg_type: UUID }
      - { name: order_number, pg_type: TEXT, keyword_subfield: true }
      - { name: customer_id,  pg_type: UUID }
      - { name: plant,        pg_type: TEXT }
      - { name: net_value,    pg_type: NUMERIC }
      - { name: currency,     pg_type: TEXT }
      - { name: order_date,   pg_type: DATE }
      - { name: delivery_date, pg_type: DATE }
    relations:
      - { field: customer, kind: belongs_to, target: CustomerMaster,
          local_col: customer_id, foreign_col: id }
      - { field: items, kind: has_many, target: SalesOrderItem,
          local_col: id, foreign_col: order_id, order_by: line_item ASC }
```

### FI (Financial Accounting) — sketch

```yaml
  - name: GLAccount
    table: gl_accounts
    index: sap_fi_gl_account
    notify_channel: fi_gl_changes
    columns:
      - { name: id,           pg_type: UUID }
      - { name: account_number, pg_type: TEXT, keyword_subfield: true }
      - { name: description,  pg_type: TEXT }
      - { name: account_type, pg_type: TEXT, keyword_subfield: true }
      - { name: currency,     pg_type: TEXT }
      - { name: balance,      pg_type: NUMERIC }
```

No Rust code changes needed — just extend the YAML and add the matching SQL table + trigger.

---

## Movement type reference (common SAP codes)

| Code | Description |
|---|---|
| 101 | Goods receipt for purchase order |
| 102 | Reversal of GR for purchase order |
| 261 | Goods issue for production order |
| 301 | Transfer posting plant to plant |
| 551 | Scrapping |
| 601 | Goods issue for delivery (sales) |

These are stored in `material_document.movement_type` and can be filtered in GraphQL:

```graphql
query {
  list_MaterialDocument(filter: "movement_type = '101'", limit: 50) {
    doc_number
    posting_date
    quantity
    material { material_number description }
    vendor    { name }
  }
}
```
