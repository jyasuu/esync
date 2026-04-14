# 02_user_token.hurl
# A valid user JWT has a 'sub' claim.
# The products_user RLS policy: sub IS NOT NULL AND active=true.
# Seed data: 4 active (UUIDs ..001-003, ..005) + 1 inactive (..004 "Delta Thing").

POST {{gql_url}}
Authorization: Bearer {{user_token}}
Content-Type: application/json
{
  "query": "{ list_product(limit: 100) { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection

# Must see the 4 active products
jsonpath "$.data.list_product[*].id" contains "00000000-0000-0000-0000-000000000001"
jsonpath "$.data.list_product[*].id" contains "00000000-0000-0000-0000-000000000002"
jsonpath "$.data.list_product[*].id" contains "00000000-0000-0000-0000-000000000003"
jsonpath "$.data.list_product[*].id" contains "00000000-0000-0000-0000-000000000005"

# Must NOT see the inactive product (Delta Thing, id ..004)
jsonpath "$.data.list_product[*].id" not contains "00000000-0000-0000-0000-000000000004"

# All returned rows must be active
jsonpath "$.data.list_product[*].active" not contains false
