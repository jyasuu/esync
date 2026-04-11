#!/usr/bin/env python3
"""Write the three esync server configs for the OAuth2 CI job.

  /tmp/esync-jwks.yaml        — JWKS validation, require_auth: false, port 4002
  /tmp/esync-strict.yaml      — JWKS validation, require_auth: true,  port 4003
  /tmp/esync-novalidate.yaml  — validation_mode: none,                port 4004
"""

BASE = """
postgres:
  url: "postgres://esync:esync@localhost:5432/esync_test"
  pool_size: 5

elasticsearch:
  url: "http://localhost:9200"

entities:
  - name: Product
    table: products
    index: test_products_oauth2
    id_column: id
    columns:
      - name: id
        pg_type: UUID
      - name: name
        pg_type: TEXT
      - name: active
        pg_type: BOOL
      - name: price
        pg_type: NUMERIC
"""

JWKS_COMMON = """
    oauth2:
      validation_mode: jwks
      jwks_uri: "http://localhost:8888/jwks.json"
      jwks_cache_ttl_secs: 60
      required_issuer: "http://localhost:8888"
      required_audience: "esync-api"
      clock_skew_secs: 10
      rls_role_claim: "roles"
      rls_user_attributes: [sub, tenant_id, email]
"""

configs = {
    "/tmp/esync-jwks.yaml": BASE + """
graphql:
  host: "127.0.0.1"
  port: 4002
  playground: false
""" + JWKS_COMMON + "      require_auth: false\n",

    "/tmp/esync-strict.yaml": BASE + """
graphql:
  host: "127.0.0.1"
  port: 4003
  playground: false
""" + JWKS_COMMON + "      require_auth: true\n",

    "/tmp/esync-novalidate.yaml": BASE + """
graphql:
  host: "127.0.0.1"
  port: 4004
  playground: false
  oauth2:
    validation_mode: none
    require_auth: false
    rls_role_claim: "roles"
    rls_user_attributes: [sub, tenant_id, email]
""",
}

for path, content in configs.items():
    with open(path, "w") as f:
        f.write(content)
    print(f"Wrote {path}")
