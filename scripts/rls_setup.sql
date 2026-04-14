-- ============================================================
-- RLS setup script for esync OAuth2 authentication
-- Uses request.jwt.claims (::jsonb) and request.jwt.token_type
-- Compatible with PostgREST-style policies.
-- ============================================================

-- ── 1. Create esync DB roles ─────────────────────────────────────────────

-- GraphQL server role — subject to RLS (non-superuser, NOBYPASSRLS)
CREATE ROLE esync_graphql LOGIN PASSWORD 'change_me_graphql'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;

-- Indexer / CDC role — bypasses RLS (sees all rows for full indexing)
CREATE ROLE esync_indexer LOGIN PASSWORD 'change_me_indexer';
ALTER ROLE esync_indexer BYPASSRLS;

-- Grant table access to both roles
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_graphql;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_indexer;

-- ── 2. Grant SET on the two JWT GUC params (Postgres 15+) ────────────────
-- Only two parameters needed regardless of how many claims your IdP issues.
GRANT SET ON PARAMETER "request.jwt.claims"     TO esync_graphql;
GRANT SET ON PARAMETER "request.jwt.token_type" TO esync_graphql;

-- ── 3. Example: multi-tenant products table ───────────────────────────────

ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;

-- Users see only their own tenant's active products.
-- tenant_id comes from a custom claim (add via Keycloak mapper or IdP config).
CREATE POLICY products_user ON products
  FOR SELECT
  USING (
    current_setting('request.jwt.token_type', true) = 'user'
    AND tenant_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
    )
  );

-- Authenticated users can only insert into their own tenant.
CREATE POLICY products_user_write ON products
  FOR INSERT
  WITH CHECK (
    current_setting('request.jwt.token_type', true) = 'user'
    AND tenant_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
    )
  );

-- Admin service account: sees all rows.
-- Checks both Keycloak (realm_access.roles) and flat-roles IdPs.
CREATE POLICY products_admin ON products
  FOR ALL
  USING (
    current_setting('request.jwt.token_type', true) = 'client_credentials'
    AND (
      current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'
      OR
      current_setting('request.jwt.claims', true)::jsonb -> 'roles' ? 'admin'
    )
  );

-- Tenant-scoped service account: sees only its own tenant.
-- Uses azp (Keycloak: authorized party = client ID) as the tenant identifier.
CREATE POLICY products_service_tenant ON products
  FOR ALL
  USING (
    current_setting('request.jwt.token_type', true) = 'client_credentials'
    AND NOT (
      current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'
      OR
      current_setting('request.jwt.claims', true)::jsonb -> 'roles' ? 'admin'
    )
    AND tenant_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'azp'
    )
  );

-- ── 4. Example: per-user orders table ────────────────────────────────────

ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders FORCE ROW LEVEL SECURITY;

-- Users see only their own orders.
CREATE POLICY orders_user ON orders
  FOR SELECT
  USING (
    current_setting('request.jwt.token_type', true) = 'user'
    AND user_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
    )
  );

-- Service accounts see all orders.
CREATE POLICY orders_service ON orders
  FOR ALL
  USING (
    current_setting('request.jwt.token_type', true) = 'client_credentials'
  );

-- ── 5. Helper: inspect current JWT context (for debugging) ───────────────

CREATE OR REPLACE FUNCTION current_jwt_context()
RETURNS TABLE(key text, value text)
LANGUAGE sql STABLE AS $$
  SELECT 'token_type', current_setting('request.jwt.token_type', true)
  UNION ALL
  SELECT 'sub',        current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
  UNION ALL
  SELECT 'azp',        current_setting('request.jwt.claims', true)::jsonb ->> 'azp'
  UNION ALL
  SELECT 'realm_roles',
    (current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' ->> 'roles')
  UNION ALL
  SELECT 'raw_claims', current_setting('request.jwt.claims', true);
$$;

-- Usage: SELECT * FROM current_jwt_context();
