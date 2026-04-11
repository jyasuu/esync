# 01_anonymous.hurl
# Anonymous requests (no Authorization header) are accepted when require_auth: false.
#
# RLS setup:
#   - products_deny_default: AS RESTRICTIVE USING (false)  ← catches everything
#   - products_user:         USING (token_type='user' AND active=true)
#   - products_admin:        USING (token_type='client_credentials' AND role='admin')
#   - products_tenant_reader: USING (token_type='client_credentials' AND role='tenant-reader' AND active=true)
#
# For anonymous requests, rls.token_type is '' (unset).
# The RESTRICTIVE policy USING (false) fires for all rows → result is empty.

POST {{gql_url}}
Content-Type: application/json
{
  "query": "{ list_product { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection
jsonpath "$.data.list_product" isEmpty
