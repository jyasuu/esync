# esync

A Rust CLI that bridges **PostgreSQL → GraphQL → Elasticsearch**.

```
esync serve          # Dynamic GraphQL API over Postgres
esync index          # Bulk-rebuild ES indices
esync watch          # Real-time CDC via LISTEN/NOTIFY
esync es …           # Full native ES operations
```

---

## Table of Contents

- [Architecture](#architecture)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
  - [Entity & Column DSL](#entity--column-dsl)
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
- [CDC Setup](#cdc-setup)
- [Docker](#docker)
- [Project Layout](#project-layout)

---

## Architecture

```
┌────────────┐  SQL   ┌───────────────┐  HTTP bulk  ┌───────────────────┐
│ PostgreSQL │───────▶│  esync index  │────────────▶│  Elasticsearch    │
│            │        └───────────────┘             │                   │
│            │  LISTEN/NOTIFY                        │  indices          │
│            │───────▶┌───────────────┐─────────────│  datastreams      │
│            │        │  esync watch  │  upsert/del  │  templates + ILM  │
│            │        └───────────────┘             └───────────────────┘
│            │  SQL                                         ▲
│            │───────▶┌───────────────┐  GraphQL            │
└────────────┘        │  esync serve  │◀── clients can also │
                      │  (axum/GQL)   │    query ES directly │
                      └───────────────┘                     │
                              │          esync es search ───┘
                              ▼
                         GraphiQL :4000
```

---

## Quick Start

### 1. Start infrastructure

```bash
docker compose up -d postgres elasticsearch kibana
```

### 2. Build & run

```bash
cargo build --release

# Full initial sync
./target/release/esync index

# Start GraphQL server
./target/release/esync serve

# Start CDC watch (separate terminal)
./target/release/esync watch
```

### 3. Try GraphQL

Open http://localhost:4000/graphql in your browser (GraphiQL playground).

```graphql
query {
  list_product(limit: 5, search: "widget") {
    id
    name
    price
    active
  }
}

query {
  get_product(id: "some-uuid-here") {
    id
    name
    description
    price
  }
}
```

### 4. Search Elasticsearch directly

```bash
esync es search -i products -f examples/search-products.json
```

---

## Configuration

esync is configured via a single YAML file (default: `esync.yaml`).

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
  playground: true

entities:
  - name: Product         # GraphQL type name + ES index name source
    table: products       # Postgres table
    index: products       # ES index name
    id_column: id         # Column used as ES _id
    notify_channel: products_changes  # Postgres NOTIFY channel
    batch_size: 500
    filter: "deleted_at IS NULL"      # optional WHERE fragment
    columns:
      - name: id
        pg_type: UUID
      - name: name
        pg_type: TEXT
        keyword_subfield: true     # adds .keyword sub-field
      - name: price
        pg_type: NUMERIC
      - name: created_at
        pg_type: TIMESTAMPTZ
```

### Entity & Column DSL

| Field | Default | Description |
|-------|---------|-------------|
| `name` | required | GraphQL type name (CamelCase recommended) |
| `table` | required | Postgres source table |
| `index` | required | Target ES index name |
| `id_column` | `id` | Primary key column → ES `_id` |
| `notify_channel` | table name | Postgres `LISTEN` channel for CDC |
| `batch_size` | `500` | Rows per bulk-index request |
| `filter` | — | SQL `WHERE` fragment (applied to all operations) |
| `columns[].pg_type` | required | Drives ES mapping + GraphQL type |
| `columns[].es_type` | derived | Override the auto-derived ES type |
| `columns[].keyword_subfield` | `true` | Add `.keyword` to `text` fields |
| `columns[].graphql` | `true` | Expose in GraphQL schema |
| `columns[].indexed` | `true` | Include in ES mapping |
| `columns[].es_extra` | `{}` | Merge verbatim into ES field definition |

### Type Mapping

Auto-derived from `pg_type` in `src/indexer/mapping.rs`:

| Postgres type | ES field type | GraphQL type |
|---------------|---------------|--------------|
| `UUID` | `keyword` | `String` |
| `TEXT`, `VARCHAR` | `text` + `.keyword` | `String` |
| `INT2`, `INT4` | `integer` | `Int` |
| `INT8` | `long` | `Float` |
| `FLOAT4` | `float` | `Float` |
| `FLOAT8` | `double` | `Float` |
| `NUMERIC` | `scaled_float` (×100) | `Float` |
| `BOOL` | `boolean` | `Boolean` |
| `TIMESTAMPTZ`, `TIMESTAMP` | `date` | `String` |
| `DATE` | `date` | `String` |
| `JSONB`, `JSON` | `object` | `String` |

---

## Commands

### serve

Start the GraphQL HTTP server (default `:4000`).

```bash
esync serve
esync serve --port 8080
esync serve --no-playground
```

Each entity in `esync.yaml` gets two generated queries:

- `list_<entity>(limit, offset, search)` — paginated list with optional full-text search
- `get_<entity>(id!)` — fetch one record by id

### index

Bulk-rebuild one or more ES indices from Postgres.

```bash
esync index                          # all entities
esync index -e Product               # one entity
esync index -e Product -e Order      # multiple
```

- Drops and recreates the index each run (use `--recreate=false` to skip delete)
- Shows a progress bar with ETA
- Streams in configurable `batch_size` chunks

### watch (CDC)

Subscribe to Postgres `LISTEN/NOTIFY` channels and keep ES in sync in real time.

```bash
esync watch                 # all entities
esync watch -e Product      # one entity
```

Payload format sent by the trigger:

```json
{ "op": "INSERT", "id": "uuid-here", "row": { "id": "...", "name": "..." } }
```

If `row` is absent (lightweight trigger), esync fetches the row from Postgres automatically.

### es index

```bash
esync es index list                            # all indices
esync es index list "products*"                # wildcard
esync es index get products
esync es index create products -f body.json
esync es index delete products
esync es index mappings products
esync es index put-mappings products -f mappings.json
```

### es doc

```bash
esync es doc get  products <uuid>
esync es doc put  products <uuid> -f doc.json
esync es doc delete products <uuid>
```

### es search

```bash
esync es search -i products -f examples/search-products.json
```

The file is a standard [Elasticsearch Query DSL](https://www.elastic.co/guide/en/elasticsearch/reference/current/query-dsl.html) JSON body.

### es datastream

```bash
esync es datastream list
esync es datastream list "logs-*"
esync es datastream create logs-esync-default
esync es datastream delete logs-esync-default
```

### es template

Manage [index templates](https://www.elastic.co/guide/en/elasticsearch/reference/current/index-templates.html):

```bash
esync es template get  esync-default
esync es template put  esync-default -f examples/template.json
esync es template delete esync-default
```

### es policy

Manage [ILM policies](https://www.elastic.co/guide/en/elasticsearch/reference/current/ilm-policy-definition.html):

```bash
esync es policy get    esync-default-policy
esync es policy put    esync-default-policy -f examples/ilm-policy.json
esync es policy delete esync-default-policy
```

---

## GraphQL API

### List with search & pagination

```graphql
query Products($q: String) {
  list_product(limit: 20, offset: 0, search: $q) {
    id
    name
    price
    stock
    active
    created_at
  }
}
```

### Get by ID

```graphql
query GetProduct($id: String!) {
  get_product(id: $id) {
    id
    name
    description
    price
  }
}
```

The GraphQL schema is fully dynamic — adding a new entity to `esync.yaml` and
restarting `esync serve` exposes it immediately with no code changes.

---

## CDC Setup

The `scripts/init.sql` ships a reusable trigger function `esync_cdc_notify()`.
Attach it to any table in one statement:

```sql
-- Syntax: esync_cdc_notify('<channel_name>')
CREATE TRIGGER orders_cdc
AFTER INSERT OR UPDATE OR DELETE ON orders
FOR EACH ROW EXECUTE FUNCTION esync_cdc_notify('orders_changes');
```

Then add the channel to your entity config:

```yaml
- name: Order
  notify_channel: orders_changes
  ...
```

For **high-volume tables** you can switch to a lightweight trigger that omits
`row` from the payload (esync will fetch from DB):

```sql
CREATE OR REPLACE FUNCTION esync_cdc_notify_slim()
RETURNS TRIGGER AS $$
BEGIN
  PERFORM pg_notify(TG_ARGV[0],
    json_build_object('op', TG_OP, 'id', (CASE WHEN TG_OP='DELETE' THEN OLD ELSE NEW END).id)::TEXT
  );
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;
```

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
├── esync.yaml                    example config
├── .env.example
├── examples/
│   ├── template.json             ES index template
│   ├── datastream-template.json  ES datastream template
│   ├── ilm-policy.json           ILM lifecycle policy
│   └── search-products.json      ES DSL query example
├── scripts/
│   └── init.sql                  Postgres schema + CDC trigger + seed
└── src/
    ├── main.rs                   CLI entry point (clap)
    ├── config.rs                 YAML config types
    ├── db.rs                     sqlx helpers
    ├── elastic.rs                Reqwest-based ES client
    ├── graphql/
    │   └── mod.rs                Dynamic async-graphql schema
    ├── indexer/
    │   ├── mod.rs                Batch rebuild logic
    │   └── mapping.rs            PG type → ES mapping derivation
    └── commands/
        ├── mod.rs
        ├── serve.rs              `esync serve`
        ├── index.rs              `esync index`
        ├── watch.rs              `esync watch` (CDC)
        └── es.rs                 `esync es *`
```
