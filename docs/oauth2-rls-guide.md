# OAuth2 Authentication + PostgreSQL RLS — Developer Guide

## Overview

esync now supports OAuth2/JWT authentication on the GraphQL endpoint.
Every request is validated, claims are extracted, and mapped to
`SET LOCAL rls.*` parameters inside each Postgres transaction — so
your Row Level Security policies can enforce fine-grained access control
without any application-layer filtering.

```
GraphQL client
    │  Bearer <token>
    ▼
┌──────────────────────────────┐
│  Axum middleware             │
│  TokenValidator              │  ← validates JWT or introspects
│  → AuthContext               │
└──────────────┬───────────────┘
               │ ctx.data::<AuthContext>()
               ▼
┌──────────────────────────────┐
│  GraphQL resolver            │
│  auth.rls_params(cfg)        │  ← builds SET LOCAL statements
│  db::fetch_rows_rls(...)     │
└──────────────┬───────────────┘
               │ BEGIN
               │ SET LOCAL rls.token_type = 'user'
               │ SET LOCAL rls.user_id    = 'abc-123'
               │ SET LOCAL rls.tenant_id  = 'acme'
               │ SELECT ...  ← Postgres RLS policy fires here
               │ COMMIT
               ▼
         PostgreSQL
```

---

## Configuration

Add an `oauth2` block inside `graphql:` in `esync.yaml`.

### Option A — JWKS (recommended for production)

```yaml
graphql:
  host: "0.0.0.0"
  port: 4000
  playground: true

  oauth2:
    validation_mode: jwks
    jwks_uri: "https://auth.example.com/.well-known/jwks.json"
    jwks_cache_ttl_secs: 300

    required_issuer:   "https://auth.example.com/"
    required_audience: "esync-api"
    clock_skew_secs: 30

    require_auth: true

    # Client-credential tokens
    rls_role_claim: "roles"

    # User tokens
    rls_user_attributes:
      - sub
      - tenant_id
      - email
      - department
```

### Option B — Token introspection (opaque tokens / Keycloak)

```yaml
graphql:
  oauth2:
    validation_mode: introspect
    introspect_endpoint: "https://auth.example.com/oauth/introspect"
    client_id:     "esync-service"
    client_secret: "${OAUTH2_CLIENT_SECRET}"
    required_issuer: "https://auth.example.com/"
    require_auth: false
    rls_role_claim: "roles"
    rls_user_attributes: [sub, tenant_id]
```

### Option C — Disabled (default, fully backward-compatible)

Omit the `oauth2:` block entirely. All requests proceed as anonymous.

---

## Token-type detection

esync auto-detects machine vs. user tokens:

| Signal | Result |
|---|---|
| `gty`/`grant_type` claim contains `"client_credentials"` | client_credentials |
| `token_type_claim` config matches `"client_credentials"` | client_credentials |
| `client_id` present AND no user-identity claims | client_credentials |
| Anything else | user |

---

## RLS variables injected per token type

### Client-credential token

| Postgres variable | Source |
|---|---|
| `rls.token_type` | `"client_credentials"` |
| `rls.client_id` | `client_id` claim (fallback: `sub`) |
| `rls.role` | first value of `rls_role_claim` / first word of `scope` |

### User token

| Postgres variable | Source |
|---|---|
| `rls.token_type` | `"user"` |
| `rls.user_id` | `sub` claim |
| `rls.<attr>` | each item in `rls_user_attributes` |

---

## PostgreSQL setup

```sql
-- 1. Enable RLS
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;

-- 2. Service account: admin sees all, tenant-scoped client sees own rows
CREATE POLICY products_service ON products FOR ALL USING (
  current_setting('rls.token_type', true) = 'client_credentials'
  AND (
    current_setting('rls.role', true) = 'admin'
    OR tenant_id = current_setting('rls.client_id', true)
  )
);

-- 3. User: sees only their tenant
CREATE POLICY products_user ON products FOR SELECT USING (
  current_setting('rls.token_type', true) = 'user'
  AND tenant_id = current_setting('rls.tenant_id', true)
);

-- 4. Grant SET on rls.* params (Postgres 15+)
GRANT SET ON PARAMETER "rls.token_type" TO esync_role;
GRANT SET ON PARAMETER "rls.user_id"    TO esync_role;
GRANT SET ON PARAMETER "rls.tenant_id"  TO esync_role;
GRANT SET ON PARAMETER "rls.client_id"  TO esync_role;
GRANT SET ON PARAMETER "rls.role"       TO esync_role;

-- 5. Indexer role bypasses RLS (separate credentials recommended)
ALTER ROLE esync_indexer BYPASSRLS;
```

---

## WebSocket subscriptions

```json
{ "type": "connection_init", "payload": { "Authorization": "Bearer <token>" } }
```

Token is validated once at connect time; the `AuthContext` is reused for all events on that connection.

---

## Security notes

| Topic | Recommendation |
|---|---|
| `validation_mode: none` | Dev/test only. Never in production. |
| `require_auth: false` | Safe only when RLS denies anonymous by default. |
| `clock_skew_secs` | Keep ≤ 60 s. Default 30. |
| `client_secret` | Use env var: `client_secret: "${OAUTH2_CLIENT_SECRET}"` |
| DB role for GraphQL | Non-BYPASSRLS; let RLS do the filtering. |
| DB role for indexer | BYPASSRLS; use separate `postgres.url`. |
