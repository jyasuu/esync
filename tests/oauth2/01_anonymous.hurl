# 01_anonymous.hurl
# Anonymous requests (no Authorization header) are accepted when require_auth: false,
# but RLS returns zero rows.
#
# How it works:
#   - No Authorization header → AuthContext::anonymous() → token_type = "anonymous"
#   - SET LOCAL rls.token_type = 'anonymous' is executed in the transaction
#   - No permissive RLS policy matches token_type='anonymous'
#   - Postgres FORCE ROW LEVEL SECURITY default: deny when no policy matches
#   - Result: empty list
#
# Note: we do NOT use AS RESTRICTIVE USING (false) — that would block all rows
# for ALL roles including user/admin. The default-deny behaviour of FORCE RLS
# is sufficient when the application role is non-superuser (esync_app).

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
