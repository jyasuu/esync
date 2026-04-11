# 01_anonymous.hurl
# Anonymous requests (no Authorization header) are accepted when require_auth: false,
# but RLS blocks all rows.
#
# What happens:
#   - No Authorization header → AuthContext::anonymous() → token_type = "anonymous"
#   - rls_params() emits SET LOCAL rls.token_type = 'anonymous'
#   - RESTRICTIVE policy products_deny_default: USING (false) fires for every row
#   - No permissive policy matches token_type='anonymous'
#   - Result: empty list (RLS deny-all)
#
# This requires the GraphQL server to connect as a non-superuser role (esync_app).
# Superusers always bypass RLS regardless of FORCE ROW LEVEL SECURITY.

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
