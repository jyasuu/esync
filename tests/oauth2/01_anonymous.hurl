# 01_anonymous.hurl
# Anonymous requests (no Authorization header) are accepted when require_auth: false.
# request.jwt.claims is set to '{}' (empty object).
#
# RLS: the products_user policy requires sub IS NOT NULL.
# An empty claims object has no sub → policy does not match → zero rows.

POST {{gql_url}}
Content-Type: application/json
{
  "query": "{ list_product { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection
jsonpath "$.data.list_product" count == 0
