# 09_jwks_rotation.hurl
# After JWKS is rotated to contain only kid=test-key-2:
#
#   - Token signed with new key (kid=test-key-2) → 200  (server fetches fresh JWKS)
#   - Token signed with old key (kid=test-key-1) → 401  (kid removed from JWKS)

# ── New key accepted ──────────────────────────────────────────────────────
POST {{gql_url}}
Authorization: Bearer {{rotated_token}}
Content-Type: application/json
{
  "query": "{ list_product(limit: 100) { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection

---

# ── Old key rejected after rotation ──────────────────────────────────────
POST {{gql_url}}
Authorization: Bearer {{user_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401
