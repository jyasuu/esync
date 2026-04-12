-- ============================================================
-- RLS setup script for esync OAuth2 authentication
-- Adapt table names, roles, and claim attributes to your schema
-- ============================================================

-- ── 1. Create esync DB roles ─────────────────────────────────────────────

-- GraphQL server role — subject to RLS
CREATE ROLE esync_graphql LOGIN PASSWORD 'change_me_graphql';

-- Indexer / CDC role — bypasses RLS (sees all rows for full indexing)
CREATE ROLE esync_indexer LOGIN PASSWORD 'change_me_indexer';
ALTER ROLE esync_indexer BYPASSRLS;

-- Grant table access to both roles
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_graphql;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO esync_indexer;

-- ── 2. Allow SET LOCAL rls.* (Postgres 15+) ──────────────────────────────
-- Repeat for each rls_user_attributes + rls_role_claim you configure.

GRANT SET ON PARAMETER "rls.token_type" TO esync_graphql;
GRANT SET ON PARAMETER "rls.user_id"    TO esync_graphql;
GRANT SET ON PARAMETER "rls.client_id"  TO esync_graphql;
GRANT SET ON PARAMETER "rls.role"       TO esync_graphql;
GRANT SET ON PARAMETER "rls.tenant_id"  TO esync_graphql;
GRANT SET ON PARAMETER "rls.email"      TO esync_graphql;
GRANT SET ON PARAMETER "rls.department" TO esync_graphql;

-- ── 3. Example: multi-tenant products table ───────────────────────────────

-- IMPORTANT: Set NOBYPASSRLS on the application role BEFORE FORCE RLS.
-- Table owners bypass RLS by default; FORCE only takes effect once the role
-- attribute is cleared.  Do this for the role that your GraphQL server uses.
ALTER ROLE esync_graphql NOBYPASSRLS;

ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;

-- No RESTRICTIVE policy needed.
-- With FORCE ROW LEVEL SECURITY, Postgres denies all rows by default when
-- no permissive policy matches. Anonymous sessions (rls.token_type='anonymous')
-- don't match any policy below → zero rows returned automatically.
-- RESTRICTIVE USING (false) would incorrectly block all roles including the
-- permissive ones below.

-- Service account with role 'admin' sees everything.
CREATE POLICY products_admin ON products
  FOR ALL
  USING (
    current_setting('rls.token_type', true) = 'client_credentials'
    AND current_setting('rls.role', true) = 'admin'
  );

-- Service account with any other role sees only its own tenant's rows.
CREATE POLICY products_service_tenant ON products
  FOR ALL
  USING (
    current_setting('rls.token_type', true) = 'client_credentials'
    AND current_setting('rls.role', true) != 'admin'
    AND tenant_id::text = current_setting('rls.client_id', true)
  );

-- Authenticated users see their own tenant's rows only (read).
CREATE POLICY products_user ON products
  FOR SELECT
  USING (
    current_setting('rls.token_type', true) = 'user'
    AND tenant_id::text = current_setting('rls.tenant_id', true)
  );

-- Authenticated users can only write their own tenant's rows.
CREATE POLICY products_user_write ON products
  FOR INSERT
  WITH CHECK (
    current_setting('rls.token_type', true) = 'user'
    AND tenant_id::text = current_setting('rls.tenant_id', true)
  );

-- ── 4. Example: per-user orders table ────────────────────────────────────

ALTER TABLE orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE orders FORCE ROW LEVEL SECURITY;

CREATE POLICY orders_user ON orders
  FOR SELECT
  USING (
    current_setting('rls.token_type', true) = 'user'
    AND user_id::text = current_setting('rls.user_id', true)
  );

CREATE POLICY orders_service ON orders
  FOR ALL
  USING (current_setting('rls.token_type', true) = 'client_credentials');

-- ── 5. Helper: inspect current RLS context (for debugging) ───────────────

CREATE OR REPLACE FUNCTION current_rls_context()
RETURNS TABLE(key text, value text)
LANGUAGE sql STABLE AS $$
  SELECT unnest(ARRAY[
    'rls.token_type', 'rls.user_id', 'rls.client_id',
    'rls.role', 'rls.tenant_id', 'rls.email'
  ]),
  unnest(ARRAY[
    current_setting('rls.token_type', true),
    current_setting('rls.user_id',    true),
    current_setting('rls.client_id',  true),
    current_setting('rls.role',       true),
    current_setting('rls.tenant_id',  true),
    current_setting('rls.email',      true)
  ]);
$$;

-- Usage: SELECT * FROM current_rls_context();
