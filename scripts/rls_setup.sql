-- ============================================================
-- RLS setup script for esync OAuth2 authentication
-- Uses only request.jwt.claims (::jsonb) — no token_type param.
-- Compatible with PostgREST-style policies.
-- ============================================================

-- ── 1. Create esync DB roles ─────────────────────────────────────────────

-- GraphQL server role — subject to RLS (non-superuser, NOBYPASSRLS)
CREATE ROLE esync_graphql LOGIN PASSWORD 'change_me_graphql'
  NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;

-- Indexer / CDC role — bypasses RLS (sees all rows for full indexing)
CREATE ROLE esync_indexer LOGIN PASSWORD 'change_me_indexer';
ALTER ROLE esync_indexer BYPASSRLS;

-- Grant table access
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_graphql;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_indexer;

-- ── 2. Grant SET on request.jwt.claims (Postgres 15+) ────────────────────
GRANT SET ON PARAMETER "request.jwt.claims" TO esync_graphql;

-- ── 3. Example: multi-tenant products table ───────────────────────────────

ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;

-- Authenticated users (has a sub claim) see only their own tenant's active products.
CREATE POLICY products_user ON products
  FOR SELECT
  USING (
    current_setting('request.jwt.claims', true)::jsonb ->> 'sub' IS NOT NULL
    AND tenant_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
    )
  );

-- Write: users can only insert into their own tenant.
CREATE POLICY products_user_write ON products
  FOR INSERT
  WITH CHECK (
    current_setting('request.jwt.claims', true)::jsonb ->> 'sub' IS NOT NULL
    AND tenant_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
    )
  );

-- Admin service account (Keycloak realm role 'admin') sees all rows.
CREATE POLICY products_admin ON products
  FOR ALL
  USING (
    current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'
    OR
    current_setting('request.jwt.claims', true)::jsonb -> 'roles' ? 'admin'
  );

-- Tenant-scoped service account: sees only its own tenant's rows (azp = client ID).
CREATE POLICY products_service_tenant ON products
  FOR ALL
  USING (
    NOT (
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
    user_id::text = (
      current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
    )
  );

-- Service accounts (have azp, no sub-based user ownership) see all orders.
CREATE POLICY orders_service ON orders
  FOR ALL
  USING (
    current_setting('request.jwt.claims', true)::jsonb ->> 'azp' IS NOT NULL
    AND current_setting('request.jwt.claims', true)::jsonb ->> 'azp' != ''
  );

-- ── 5. Helper: inspect current JWT context (for debugging) ───────────────

CREATE OR REPLACE FUNCTION current_jwt_context()
RETURNS TABLE(key text, value text)
LANGUAGE sql STABLE AS $$
  SELECT 'sub',         current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
  UNION ALL
  SELECT 'azp',         current_setting('request.jwt.claims', true)::jsonb ->> 'azp'
  UNION ALL
  SELECT 'realm_roles', (current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' ->> 'roles')
  UNION ALL
  SELECT 'raw_claims',  current_setting('request.jwt.claims', true);
$$;

-- Usage: SELECT * FROM current_jwt_context();
