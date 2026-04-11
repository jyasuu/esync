# 08_validation_none.hurl
# Server running with validation_mode: none (port 4004).
# Signature is NOT verified — but claims are still extracted and RLS vars are set.
# We send a structurally valid JWT with an obviously fake signature.
# The user-token payload has email=dev@test.com, tenant_id=dev-tenant.

# ── Fake-sig token accepted, RLS still applies ────────────────────────────
POST {{gql_novalidate_url}}
Authorization: Bearer {{dev_token}}
Content-Type: application/json
{
  "query": "{ list_product(limit: 100) { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection
# RLS user policy fires (token_type='user', active=true only)
jsonpath "$.data.list_product[*].id" not includes "00000000-0000-0000-0000-000000000004"
jsonpath "$.data.list_product[*].active" not includes false

---

# ── No token in none-mode with require_auth:false → still OK ─────────────
POST {{gql_novalidate_url}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isEmpty
