# esync

A Rust CLI that syncs **PostgreSQL → Elasticsearch** with a config-driven approach: define your entities, relations, and computed fields in YAML — esync handles bulk indexing, real-time CDC, and a dynamic GraphQL API over Postgres with no code changes required.

```
esync serve    # Dynamic GraphQL API over Postgres (relations, filtering, pagination)
esync index    # Bulk-rebuild ES indices with denormalized search_text fields
esync watch    # Real-time CDC sync via Postgres LISTEN/NOTIFY
esync es …     # Native ES operations (indices, docs, templates, ILM, datastreams)
```

---

## Table of Contents

- [Architecture](#architecture)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
  - [Entity & Column DSL](#entity--column-dsl)
  - [Relations](#relations)
  - [search\_text — Denormalized Full-Text Field](#search_text--denormalized-full-text-field)
  - [search — ES-Backed GraphQL Search](#search--es-backed-graphql-search)
  - [Type Mapping](#type-mapping)
- [Commands](#commands)
  - [serve](#serve)
  - [index](#index)
  - [watch (CDC)](#watch-cdc)
  - [es index](#es-index)
  - [es doc](#es-doc)
  - [es search](#es-search)
  - [es datastream](#es-datastream)
  - [es template](#es-template)
  - [es policy](#es-policy)
- [GraphQL API](#graphql-api)
  - [list\_\<entity\>](#list_entity)
  - [get\_\<entity\>](#get_entity)
  - [Relations in GraphQL](#relations-in-graphql)
  - [search\_\<entity\>](#search_entity)
- [CDC Setup](#cdc-setup)
- [Integration Tests](#integration-tests)
- [Docker](#docker)
- [Project Layout](#project-layout)

---

## Architecture

```
┌─────────────┐  SQL   ┌──────────────────────────────────────────────┐
│ PostgreSQL  │──────▶ │  esync index                                 │
│             │        │  • fetches rows in batches                   │
│             │        │  • builds search_text from relations         │
│             │        │  • bulk-writes to Elasticsearch              │
│             │        └──────────────────────────────────────────────┘
│             │                          │ HTTP bulk
│             │                          ▼
│             │  LISTEN/NOTIFY  ┌──────────────────────┐
│             │───────────────▶ │  esync watch (CDC)   │
│             │                 │  • per-row upsert/del │
│             │                 │  • rebuilds           │
│             │                 │    search_text live   │
│             │                 └──────────────────────┘
│             │                          │ HTTP upsert/delete
│             │                          ▼
│             │  SQL   ┌──────────────────────────────────────────────┐
│             │──────▶ │  esync serve (axum + async-graphql)          │
└─────────────┘        │  • dynamic schema from esync.yaml            │
                       │  • list_*, get_* queries over Postgres       │
                       │  • belongs_to / has_many / many_to_many      │
                       │  • search_* queries via Elasticsearch        │
                       └──────────────────────────────────────────────┘
                                         │
                                  GraphiQL :4000
```

**Key separation of concerns:**

| Layer | Talks to | Purpose |
|-------|----------|---------|
| `esync index` / `watch` | Postgres + ES | Write path — sync data, build search_text |
| `esync serve` (GraphQL) | Postgres only | Read path — relational queries, live data |
| `search_*` GQL fields | ES (optional) | Read path — full-text search over indexed data |

---

## Quick Start

### 1. Start infrastructure

```bash
docker compose up -d postgres elasticsearch kibana
```

### 2. Build & run

```bash
cargo build --release

# Full initial sync (builds search_text fields, writes to ES)
./target/release/esync index

# Start GraphQL server
./target/release/esync serve

# Start CDC watch in a separate terminal
./target/release/esync watch
```

### 3. Try the GraphQL API

Open http://localhost:4000/graphql (GraphiQL playground).

```graphql
# List with pagination and full-text filter
query {
  list_product(limit: 10, search: "widget") {
    id  name  price  stock  active
  }
}

# Traverse relations — no foreign keys required
query {
  get_customer(id: "uuid-here") {
    id  name  email
    orders { id  status  total }
    tags  { label }
  }
}

# ES-backed full-text search with live PG enrichment
query {
  search_product(q: "wireless headphones", limit: 10) {
    total  took
    items {
      _score
      _highlight { name  description }
      id  name  price
      stock        # fetched live from Postgres on every search
    }
  }
}
```

### 4. Search Elasticsearch directly

```bash
esync es search -i products -f examples/search-products.json
```

---

## Configuration

All behaviour is driven by a single YAML file (default: `esync.yaml`).
Pass a different path with `--config path/to/config.yaml` or set `ESYNC_CONFIG`.

```yaml
postgres:
  url: "postgres://user:pass@host:5432/dbname"
  pool_size: 10           # optional, default 10

elasticsearch:
  url: "http://localhost:9200"
  username: elastic       # optional
  password: changeme      # optional

graphql:
  host: "0.0.0.0"
  port: 4000
  playground: true        # enable GraphiQL UI

entities:
  - name: Product         # GraphQL type name (CamelCase recommended)
    table: products       # Postgres source table
    index: products       # ES index name
    id_column: id         # Column used as ES document _id (default: id)
    notify_channel: products_changes  # Postgres NOTIFY channel (default: table name)
    batch_size: 500       # Rows per bulk-index request (default: 500)
    filter: "deleted_at IS NULL"      # Optional SQL WHERE fragment
    columns: [ … ]        # See below
    relations: [ … ]      # See Relations
    search_text: { … }    # See search_text
    search: { … }         # See search (ES-backed GQL search)
```

### Entity & Column DSL

| Field | Default | Description |
|-------|---------|-------------|
| `name` | required | GraphQL type name |
| `table` | required | Postgres source table |
| `index` | required | Target ES index name |
| `id_column` | `"id"` | Primary key column → ES `_id` |
| `notify_channel` | table name | Postgres `LISTEN` channel for CDC |
| `batch_size` | `500` | Rows per bulk-index request |
| `filter` | — | SQL `WHERE` fragment applied to all operations |
| `columns[].pg_type` | required | Drives ES mapping and GraphQL scalar type |
| `columns[].es_type` | derived | Override the auto-derived ES type |
| `columns[].keyword_subfield` | `true` | Adds `.keyword` sub-field to `text` mappings |
| `columns[].graphql` | `true` | Expose this column in GraphQL schema |
| `columns[].indexed` | `true` | Include this column in ES mapping |
| `columns[].es_extra` | `{}` | Extra properties merged verbatim into the ES field definition |

---

### Relations

Relations expose joined data as GraphQL fields and are resolved lazily against Postgres — **no foreign keys required**. Three kinds are supported:

#### belongs_to — many-to-one

```yaml
relations:
  - field: customer          # GraphQL field name on this type
    kind: belongs_to
    target: Customer         # name of the target entity
    local_col: customer_id   # column on THIS table holding the FK value
    foreign_col: id          # column on the TARGET table to match (default: id)
```

#### has_many — one-to-many

```yaml
relations:
  - field: orders
    kind: has_many
    target: Order
    local_col: id            # THIS entity's PK
    foreign_col: customer_id # column on the TARGET table pointing back here
    order_by: placed_at DESC # optional ORDER BY (default: natural order)
    limit: 50                # optional row cap (default: 100)
    filter: "deleted_at IS NULL"  # optional extra WHERE fragment
```

#### many_to_many — via join table

```yaml
relations:
  - field: tags
    kind: many_to_many
    target: Tag
    join_table: product_tags      # the junction table
    local_col: product_id         # junction table column pointing to THIS entity
    foreign_col: tag_id           # junction table column pointing to TARGET entity
    target_id_col: id             # PK on the target table (default: id)
```

Relations nest freely — a query can traverse `order → customer → tags` in a single request.

---

### search\_text — Denormalized Full-Text Field

`search_text` builds a computed plain-text string during `esync index` and `esync watch` by concatenating columns from the entity's own table **and** from related tables. The result is stored as a single `text` field in ES, ready for full-text search — with no Elasticsearch access at query time.

```yaml
search_text:
  field: search_text        # ES field name (default: "search_text")
  separator: " "            # join separator (default: single space)
  sources:
    # Own columns — just reference by name
    - column: name
    - column: description

    # belongs_to relation — pulls columns from the joined row
    - relation: category      # must be defined in relations:
      columns: [name, slug]

    # has_many relation — joins all matched rows' values
    - relation: order_items
      columns: [sku, product_name]

    # many_to_many relation — joins all matched rows' values
    - relation: tags
      columns: [label]
```

**How it works:**

1. During `esync index`, for each row the builder fires one SQL query per relation source
2. All text parts are joined with `separator` and stored as `search_text` in the ES document
3. During `esync watch`, the field is rebuilt fresh from Postgres on every INSERT/UPDATE before the ES document is written — so related data (e.g. new tags) stays in sync automatically

The resulting ES document looks like:

```json
{
  "id": "uuid",
  "name": "Alpha Widget",
  "price": 9.99,
  "search_text": "Alpha Widget first widget Gadgets gadgets Acme Corp taipei vip wholesale"
}
```

You can then search with a plain ES `match` query on `search_text`:

```bash
esync es search -i products -q '{"query":{"match":{"search_text":"wireless headphones"}}}'
```

---

### search — ES-Backed GraphQL Search

When `search.enabled: true`, a `search_<entity>` query is added to the GraphQL schema. It runs a multi-field full-text query against Elasticsearch and optionally enriches hits with live data from Postgres.

```yaml
search:
  enabled: true
  fields:                        # ES fields to search across (boosts supported)
    - field: "name^3"
    - field: description
    - field: search_text         # include the denormalized field for best recall
  highlight:                     # fields to return ES highlighted snippets for
    - name
    - description
  live_columns:                  # re-fetched from Postgres after every search
    - stock                      # always fresh, bypasses ES _source staleness
    - updated_at
  enrich:                        # relations to resolve from Postgres on each hit
    - orders                     # has_many — resolved in one batch query
    - tags                       # many_to_many — resolved in one batch query
  cross_index:                   # also search these other entity indices
    - Article
```

**The four steps of a `search_*` resolver:**

1. Build a `multi_match` + optional `bool`/`filter` ES query from arguments
2. Execute the ES search — get back hits with `_score`, `_source`, `highlight`
3. Re-fetch `live_columns` from Postgres in one `WHERE id = ANY($1)` query
4. Batch-resolve each `enrich` relation from Postgres (one query per relation, not per hit)

---

### Type Mapping

Auto-derived from `pg_type` — can be overridden with `es_type`:

| Postgres type | ES field type | GraphQL type |
|---------------|---------------|--------------|
| `UUID` | `keyword` | `String` |
| `TEXT`, `VARCHAR` | `text` + `.keyword` | `String` |
| `INT2`, `INT4` | `integer` | `Int` |
| `INT8` | `long` | `Float` |
| `FLOAT4` | `float` | `Float` |
| `FLOAT8` | `double` | `Float` |
| `NUMERIC` | `scaled_float` (factor 100) | `Float` |
| `BOOL` | `boolean` | `Boolean` |
| `TIMESTAMPTZ`, `TIMESTAMP` | `date` | `String` |
| `DATE` | `date` | `String` |
| `JSONB`, `JSON` | `object` | `String` |

---

## Commands

### serve

Start the GraphQL HTTP server.

```bash
esync serve                    # default :4000
esync serve --port 8080
esync serve --host 127.0.0.1
esync serve --no-playground    # disable GraphiQL UI
```

The GraphQL schema is fully dynamic — adding an entity to `esync.yaml` and restarting `serve` exposes it immediately with no code changes.

### index

Bulk-rebuild one or more ES indices from Postgres.

```bash
esync index                    # all entities
esync index -e Product         # one entity
esync index -e Product -e Order
```

For each entity:
- Drops and recreates the index with derived mappings
- Adds a `text` mapping for `search_text` if configured
- Streams rows in `batch_size` chunks with a progress bar
- Builds `search_text` per row if configured (one SQL query per relation source)

### watch (CDC)

Subscribe to Postgres `LISTEN/NOTIFY` channels and keep ES in sync in real time.

```bash
esync watch                    # all entities
esync watch -e Product
```

On each `INSERT` or `UPDATE`:
- Uses the row from the NOTIFY payload, or fetches it from Postgres if absent
- Rebuilds `search_text` fresh from Postgres (so relation changes propagate)
- Upserts the ES document

On `DELETE`: removes the ES document.

### es index

```bash
esync es index list
esync es index list "products*"
esync es index get     products
esync es index create  products -f body.json
esync es index delete  products
esync es index mappings     products
esync es index put-mappings products -f mappings.json
```

### es doc

```bash
esync es doc get    products <uuid>
esync es doc put    products <uuid> -f doc.json
esync es doc delete products <uuid>
```

### es search

```bash
esync es search -i products -f examples/search-products.json
```

Accepts a standard [Elasticsearch Query DSL](https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl.html) JSON body.

### es datastream

```bash
esync es datastream list
esync es datastream list "logs-*"
esync es datastream create logs-esync-default
esync es datastream delete logs-esync-default
```

### es template

```bash
esync es template get    esync-default
esync es template put    esync-default -f examples/template.json
esync es template delete esync-default
```

### es policy

```bash
esync es policy get    esync-default-policy
esync es policy put    esync-default-policy -f examples/ilm-policy.json
esync es policy delete esync-default-policy
```

---

## GraphQL API

### list\_\<entity\>

```graphql
query {
  list_product(
    limit: 20       # default 20
    offset: 0       # default 0
    search: "widget"            # ILIKE across all TEXT/VARCHAR columns
    filter: "price > 10"        # raw SQL WHERE fragment
  ) {
    id  name  price  stock  active  created_at
  }
}
```

### get\_\<entity\>

```graphql
query {
  get_product(id: "uuid-here") {
    id  name  description  price
  }
}
```

### Relations in GraphQL

All three relation kinds are exposed as typed fields and resolve lazily via Postgres:

```graphql
query {
  get_customer(id: "uuid-here") {
    id
    name
    email

    # belongs_to: single nullable object
    # (no special config needed — declared in relations:)

    # has_many: non-null list
    orders {
      id  status  total  placed_at
    }

    # many_to_many: non-null list
    tags {
      id  label
    }
  }
}

# Nesting works to any depth
query {
  get_order(id: "uuid-here") {
    id  status
    customer {
      id  name
      tags { label }   # order → customer → tags (two hops)
    }
  }
}
```

### search\_\<entity\>

Available when `search.enabled: true` in the entity config:

```graphql
query {
  search_product(
    q: "wireless headphones"
    filter: "{\"term\":{\"active\":true}}"    # ES filter clause as JSON string
    sort:   "[{\"_score\":{\"order\":\"desc\"}},{\"price\":{\"order\":\"asc\"}}]"
    limit:  20
    offset: 0
  ) {
    total    # ES total hits count
    took     # ES query time (ms)
    items {
      _score                            # ES relevance score
      _highlight { name  description }  # highlighted snippets with <em> tags

      # All entity scalar fields (from ES _source)
      id  name  price  active

      # live_columns: always re-fetched from Postgres after search
      stock  updated_at

      # enrich: relations resolved from Postgres in batch
      orders { id  status }
      tags   { label }
    }
  }
}
```

---

## CDC Setup

`scripts/init.sql` ships the trigger function `esync_cdc_notify()`. Attach it to any table:

```sql
CREATE TRIGGER products_cdc
AFTER INSERT OR UPDATE OR DELETE ON products
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('products_changes');
```

Match the channel name in your entity config:

```yaml
- name: Product
  notify_channel: products_changes
  …
```

**Payload format** sent to esync:

```json
{ "op": "INSERT", "id": "uuid", "row": { "id": "…", "name": "…" } }
```

If `row` is absent (lightweight trigger), esync fetches the row from Postgres automatically before rebuilding `search_text` and writing to ES.

**High-volume tables** — slim trigger (omits `row`, esync fetches on demand):

```sql
CREATE OR REPLACE FUNCTION esync_cdc_notify_slim()
RETURNS TRIGGER AS $$
BEGIN
  PERFORM pg_notify(TG_ARGV[0], json_build_object(
    'op', TG_OP,
    'id', (CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END).id
  )::TEXT);
  RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$ LANGUAGE plpgsql;
```

---

## Integration Tests

Tests require a running Postgres and Elasticsearch. All 56 tests run against `esync.test.yaml`.

```bash
# Start test infra
docker compose -f docker-compose.test.yml up -d

# Set up test database
psql -U esync -d esync_test -f scripts/test/setup_test_db.sql

# Run all test suites
bash scripts/test/run_integration_tests.sh

# Or run a specific suite
cargo test --test test_mapping       # 19 unit tests — no infra needed
cargo test --test test_elastic       # 11 ES client tests
cargo test --test test_index_command # 6 tests — indexing + ES
cargo test --test test_watch_cdc     # 4 tests — CDC LISTEN/NOTIFY
cargo test --test test_graphql       # 8 tests — GQL list/get queries
cargo test --test test_relations     # 8 tests — belongs_to/has_many/many_to_many
cargo test --test test_search        # 12 tests — ES-backed search_* queries
cargo test --test test_search_text   # 8 tests — search_text denormalization
```

### Test suite summary

| Suite | Tests | What it covers |
|-------|-------|----------------|
| `test_mapping` | 19 | PG→ES type derivation, index body building |
| `test_elastic` | 11 | ES client CRUD, bulk index, error handling |
| `test_index_command` | 6 | Rebuild index, batch size, filter, row count |
| `test_watch_cdc` | 4 | NOTIFY → ES upsert/delete, ready-signal pattern |
| `test_graphql` | 8 | `list_*` + `get_*` pagination, search, filter |
| `test_relations` | 8 | All three relation kinds, nesting, null FK |
| `test_search` | 12 | `search_*` GQL: highlight, filter, live\_cols, enrich, pagination |
| `test_search_text` | 8 | Denormalized field: own cols, relations, ES searchability, CDC rebuild |

---

## Docker

```bash
# Dev: just infra
docker compose up -d postgres elasticsearch kibana

# Full stack (builds esync image)
docker compose --profile full up -d

# Logs
docker compose logs -f esync-serve
```

---

## Project Layout

```
esync/
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── docker-compose.test.yml
├── esync.yaml                        example production config
├── esync.test.yaml                   integration test config
├── .env.example
├── examples/
│   ├── template.json                 ES index template
│   ├── datastream-template.json      ES datastream template
│   ├── ilm-policy.json               ILM lifecycle policy
│   └── search-products.json          ES DSL query example
├── scripts/
│   ├── init.sql                      Postgres schema + CDC trigger
│   └── test/
│       ├── setup_test_db.sql         Test DB schema + seed procedure
│       ├── run_integration_tests.sh  Run all test suites
│       └── smoke_test.sh             Quick sanity check
└── src/
    ├── main.rs                       CLI entry point (clap)
    ├── lib.rs                        Public module exports (for integration tests)
    ├── config.rs                     YAML config types + DSL structs
    ├── db.rs                         sqlx helpers (fetch_rows, fetch_by_ids)
    ├── elastic.rs                    Reqwest-based ES HTTP client
    ├── graphql/
    │   ├── mod.rs                    Dynamic schema: list_*, get_*, relations
    │   └── search.rs                 search_* fields: ES query + PG enrichment
    ├── indexer/
    │   ├── mod.rs                    Bulk rebuild + search_text injection
    │   ├── mapping.rs                PG type → ES mapping derivation
    │   └── search_text.rs            Denormalized text field builder
    └── commands/
        ├── mod.rs
        ├── serve.rs                  `esync serve`
        ├── index.rs                  `esync index`
        ├── watch.rs                  `esync watch` (CDC + search_text rebuild)
        └── es.rs                     `esync es *`
```
