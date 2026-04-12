# 07_require_auth_strict.hurl
# Server running with require_auth: true (port 4003).
# Requests without an Authorization header must be rejected.

# ── No token → 401 ───────────────────────────────────────────────────────
POST {{gql_strict_url}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401



# ── Empty Bearer → 401 ───────────────────────────────────────────────────
POST {{gql_strict_url}}
Authorization: Bearer
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401



# ── Valid user token → 200 ────────────────────────────────────────────────
POST {{gql_strict_url}}
Authorization: Bearer {{user_token}}
Content-Type: application/json
{
  "query": "{ list_product { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection



# ── Valid admin CC token → 200 ────────────────────────────────────────────
POST {{gql_strict_url}}
Authorization: Bearer {{cc_admin_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
