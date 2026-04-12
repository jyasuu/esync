# 04_tenant_reader_cc_token.hurl
# Client-credentials token with roles=["tenant-reader"] sets rls.role='tenant-reader'.
# The products_tenant_reader RLS policy adds active=true filter.
# → same visibility as user token: no inactive rows.

POST {{gql_url}}
Authorization: Bearer {{cc_tenant_token}}
Content-Type: application/json
{
  "query": "{ list_product(limit: 100) { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection

# Must NOT see the inactive product
jsonpath "$.data.list_product[*].id" not contains "00000000-0000-0000-0000-000000000004"

# All returned rows are active
jsonpath "$.data.list_product[*].active" not contains false
