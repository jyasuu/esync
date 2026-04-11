# 03_admin_cc_token.hurl
# Client-credentials token with roles=["admin"] sets:
#   rls.token_type  = 'client_credentials'
#   rls.role        = 'admin'
#   rls.client_id   = 'svc-1'
#
# The products_admin RLS policy: FOR ALL USING (token_type='client_credentials' AND role='admin')
# → no active filter, so the inactive product (..004) is also visible.

POST {{gql_url}}
Authorization: Bearer {{cc_admin_token}}
Content-Type: application/json
{
  "query": "{ list_product(limit: 100) { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection

# Admin sees all 5 seeded products
jsonpath "$.data.list_product[*].id" includes "00000000-0000-0000-0000-000000000001"
jsonpath "$.data.list_product[*].id" includes "00000000-0000-0000-0000-000000000004"
jsonpath "$.data.list_product[*].id" includes "00000000-0000-0000-0000-000000000005"

# Specifically the inactive one must be there
jsonpath "$.data.list_product[*].active" includes false
