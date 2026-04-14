# OAuth2 Authentication + PostgreSQL RLS — Developer Guide

## Overview

esync validates OAuth2/JWT tokens and injects the full claims payload into
two Postgres GUC parameters before each query.  RLS policies use standard
`::jsonb` operators to extract whatever they need — no esync config changes
required as policies evolve.

This is the same convention used by **PostgREST**, so existing PostgREST-style
policies work without modification.

```
GraphQL client
    │  Bearer <token>
    ▼
┌──────────────────────────────┐
│  TokenValidator              │  validates JWT / introspects
│  build_auth_context()        │  detects token type
│  → AuthContext {             │
│      raw_claims: JSON string }│
└──────────────┬───────────────┘
               │ auth.rls_params(cfg)
               ▼
┌──────────────────────────────────────────────────────┐
│  BEGIN                                               │
│  SET LOCAL request.jwt.claims = '{"sub":"...","realm_access":{...},...}'
│  SELECT ... FROM products WHERE ...                  │  ← RLS policies fire
│  COMMIT                                              │
└──────────────────────────────────────────────────────┘
```

---

## Configuration

Minimal config — works with any OAuth2/OIDC provider:

```yaml
graphql:
  oauth2:
    validation_mode: jwks
    jwks_uri: "https://auth.example.com/.well-known/jwks.json"
    required_issuer: "https://auth.example.com/"
    required_audience: "esync-api"   # omit to skip audience check
    require_auth: true

    # Optional: change the GUC parameter names (defaults shown)
    # jwt_claims_param: "request.jwt.claims"   # default, PostgREST-compatible
```

That's it. Policies read directly from the JWT via `::jsonb` — no esync-specific claim config needed.

### Keycloak

```yaml
graphql:
  oauth2:
    validation_mode: jwks
    jwks_uri: "https://iam.example.com/auth/realms/service/protocol/openid-connect/certs"
    required_issuer: "https://iam.example.com/auth/realms/service"
    require_auth: true
    # No Keycloak-specific options needed — token-type detection is automatic.
```

### Validation modes

| `validation_mode` | Description |
|---|---|
| `jwks` (default) | Validate RS256 JWT signature via remote JWKS URL |
| `introspect` | RFC 7662 token introspection (for opaque tokens) |
| `none` | Skip validation — dev/test only |

---

## GUC parameter set per request

| Parameter | Value |
|---|---|
| `request.jwt.claims` | Full JWT payload as compact JSON string |

Anonymous requests (no token) set `request.jwt.claims = '{}'`.

---

## RLS policy patterns

### Extract any top-level claim

```sql
current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
current_setting('request.jwt.claims', true)::jsonb ->> 'azp'
current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
current_setting('request.jwt.claims', true)::jsonb ->> 'email'
```

### Nested claims (Keycloak)

```sql
-- Check if realm_access.roles array contains a value
current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'

-- Check if resource_access.<client>.roles contains a value
current_setting('request.jwt.claims', true)::jsonb -> 'resource_access' -> 'my-api' -> 'roles' ? 'data-reader'

-- Get first realm role as a string
current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ->> 0
```

### Complete policy examples

```sql
-- Users see only their own rows
CREATE POLICY orders_own ON orders FOR SELECT USING (
    AND user_id::text = (
    current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
  )
);

-- Multi-tenant: users see their tenant (custom claim from IdP mapper)
CREATE POLICY products_tenant ON products FOR SELECT USING (
    AND tenant_id::text = (
    current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
  )
);

-- Keycloak service account with realm role 'admin' sees everything
CREATE POLICY orders_admin ON orders FOR ALL USING (
    AND current_setting('request.jwt.claims', true)::jsonb
      -> 'realm_access' -> 'roles' ? 'admin'
);

-- Same policy, flat-roles IdP (Auth0, Azure AD)
CREATE POLICY orders_admin_flat ON orders FOR ALL USING (
    AND current_setting('request.jwt.claims', true)::jsonb -> 'roles' ? 'admin'
);

-- Combined: works for both Keycloak and flat-roles
CREATE POLICY orders_admin_any ON orders FOR ALL USING (
    AND (
    current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'
    OR
    current_setting('request.jwt.claims', true)::jsonb -> 'roles' ? 'admin'
  )
);
```

---

## PostgreSQL setup

```sql
-- Non-superuser role (NOBYPASSRLS is critical — superusers always bypass RLS)
CREATE ROLE esync_app LOGIN PASSWORD 'secret'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_app;

-- Grant SET on the two GUC params (Postgres 15+)
GRANT SET ON PARAMETER "request.jwt.claims"     TO esync_app;

-- Enable RLS
ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders FORCE ROW LEVEL SECURITY;
```

---

## WebSocket subscriptions

```json
{ "type": "connection_init", "payload": { "Authorization": "Bearer <token>" } }
```

Token validated once at connect time; the `AuthContext` is reused for all events on that connection.

---

## Debugging

```sql
-- Inspect the current JWT context from within any query/function
SELECT * FROM current_jwt_context();
-- Returns: sub, azp, realm_roles, raw_claims

-- Or manually:
SELECT
  current_setting('request.jwt.claims', true)::jsonb ->> 'sub' AS sub,
  current_setting('request.jwt.claims', true)::jsonb AS claims;
```
