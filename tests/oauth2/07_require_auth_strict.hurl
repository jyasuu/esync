# 07_require_auth_strict.hurl
# Server running with require_auth: true (port 4003).
# Requests without an Authorization header must be rejected.

# Entry 1: No token → 401
POST {{gql_strict_url}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 2: Empty Bearer → 401
POST {{gql_strict_url}}
Authorization: Bearer
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 3: Valid user token → 200
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

# Entry 4: Valid admin CC token → 200
POST {{gql_strict_url}}
Authorization: Bearer {{cc_admin_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
