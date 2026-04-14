# Keycloak Integration Guide

## How esync injects JWT claims into Postgres

esync sets **two** Postgres GUC parameters via `SET LOCAL` before each query:

| Parameter | Value | Usage |
|---|---|---|
| `request.jwt.claims` | Full JWT payload as compact JSON string | Extract any claim with `::jsonb` operators |

This is the same convention as **PostgREST** — existing PostgREST-style RLS policies work unchanged.

---

## Keycloak JWT structure

```jsonc
{
  "iss": "https://iam.example.com/auth/realms/service",
  "sub": "122a0443-3ec5-4e7f-9f76-72d1bc948ffc",
  "azp": "mda-service-3pnmx2sr",          // client ID (service account)
  "preferred_username": "service-account-mda-service-3pnmx2sr",
  "realm_access": {
    "roles": ["offline_access", "uma_authorization", "admin"]
  },
  "resource_access": {
    "my-api": { "roles": ["data-reader"] }
  }
}
```

esync auto-detects service accounts by `preferred_username` starting with `"service-account-"`.

---

## esync.yaml — minimal config

```yaml
graphql:
  oauth2:
    validation_mode: jwks
    jwks_uri: "https://iam.example.com/auth/realms/service/protocol/openid-connect/certs"
    required_issuer: "https://iam.example.com/auth/realms/service"
    require_auth: true
    # That's it. Policies use ::jsonb directly.
    # Policies use ::jsonb to extract whatever they need.
```

---

## Postgres RLS policies

### Branch on token type (fast, no JSON parsing)

```sql
current_setting('request.jwt.claims', true)::jsonb ->> 'sub' IS NOT NULL   -- authenticated user
current_setting('request.jwt.claims', true)::jsonb ->> 'azp' IS NOT NULL    -- service account
```

### Extract any claim

```sql
-- sub (user UUID)
current_setting('request.jwt.claims', true)::jsonb ->> 'sub'

-- azp (Keycloak service account client ID)
current_setting('request.jwt.claims', true)::jsonb ->> 'azp'

-- preferred_username
current_setting('request.jwt.claims', true)::jsonb ->> 'preferred_username'

-- Keycloak realm role (array contains check)
current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'

-- Keycloak client role
current_setting('request.jwt.claims', true)::jsonb -> 'resource_access' -> 'my-api' -> 'roles' ? 'data-reader'

-- Custom claim (add via Keycloak mapper)
current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
```

### Full policy examples

```sql
-- Users see only their own orders
CREATE POLICY orders_own ON orders FOR SELECT USING (
  current_setting('request.jwt.claims', true)::jsonb ->> 'sub' IS NOT NULL
  AND user_id::text = (
    current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
  )
);

-- Service account with Keycloak realm role 'admin' sees everything
CREATE POLICY orders_admin ON orders FOR ALL USING (
  current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'
);

-- Service account with client role 'data-reader' sees active orders only
CREATE POLICY orders_reader ON orders FOR SELECT USING (
  current_setting('request.jwt.claims', true)::jsonb -> 'resource_access' -> 'my-api' -> 'roles' ? 'data-reader'
);

-- Multi-tenant: users see their tenant's rows (custom claim)
CREATE POLICY products_tenant ON products FOR SELECT USING (
  current_setting('request.jwt.claims', true)::jsonb ->> 'sub' IS NOT NULL
  AND tenant_id::text = (
    current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
  )
);
```

---

## Postgres setup

```sql
-- Non-superuser role for the GraphQL server
CREATE ROLE esync_app LOGIN PASSWORD 'change_me' NOSUPERUSER NOBYPASSRLS;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_app;

-- Postgres 15+: grant SET permission for the two GUC params
GRANT SET ON PARAMETER "request.jwt.claims"     TO esync_app;

-- Enable RLS
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders FORCE ROW LEVEL SECURITY;
```

---

## Hurl test

```bash
hurl --variable "keycloak_url=https://iam.example.com" \
     --variable "realm=service" \
     --variable "client_id=mda-service-3pnmx2sr" \
     --variable "client_secret=<secret>" \
     --variable "gql_url=http://localhost:4000/graphql" \
     examples/keycloak/keycloak_cc.hurl
```
