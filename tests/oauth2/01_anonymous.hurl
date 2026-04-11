# 01_anonymous.hurl
# Verify anonymous requests (no Authorization header) are accepted when
# require_auth: false, and that RLS blocks all rows for the anonymous identity
# (no rls.* GUC vars are set, so no RLS policy matches → empty result set).

POST {{gql_url}}
Content-Type: application/json
{
  "query": "{ list_product { id name active } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_product" isCollection
# RLS: no matching policy for anonymous → empty list
jsonpath "$.data.list_product" isEmpty
