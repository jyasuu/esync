# 05_get_by_id_rls.hurl
# Verify RLS applies to get_product (single-row lookup) too, not only list_product.

# User token: get_product for an ACTIVE product → returns data
POST {{gql_url}}
Authorization: Bearer {{user_token}}
Content-Type: application/json
{
  "query": "{ get_product(id: \"00000000-0000-0000-0000-000000000001\") { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.get_product.id" == "00000000-0000-0000-0000-000000000001"
jsonpath "$.data.get_product.active" == true



# User token: get_product for the INACTIVE product → RLS hides it (null)
POST {{gql_url}}
Authorization: Bearer {{user_token}}
Content-Type: application/json
{
  "query": "{ get_product(id: \"00000000-0000-0000-0000-000000000004\") { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.get_product" isNull



# Admin CC token: get_product for the INACTIVE product → visible
POST {{gql_url}}
Authorization: Bearer {{cc_admin_token}}
Content-Type: application/json
{
  "query": "{ get_product(id: \"00000000-0000-0000-0000-000000000004\") { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.get_product.id" == "00000000-0000-0000-0000-000000000004"
jsonpath "$.data.get_product.active" == false
