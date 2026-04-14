# keycloak_cc.hurl
# Client-credentials flow against a real Keycloak realm.
#
# Variables required (pass via --variable or --variables-file):
#   keycloak_url    https://iam.example.com
#   realm           service
#   client_id       esync-svc
#   client_secret   <secret from Keycloak client Credentials tab>
#   gql_url         http://localhost:4000/graphql
#
# Run:
#   hurl --variable "keycloak_url=https://iam.example.com" \
#        --variable "realm=service" \
#        --variable "client_id=esync-svc" \
#        --variable "client_secret=<secret>" \
#        --variable "gql_url=http://localhost:4000/graphql" \
#        keycloak_cc.hurl

# ── Step 1: Fetch token from Keycloak ────────────────────────────────────
POST {{keycloak_url}}/realms/{{realm}}/protocol/openid-connect/token
Content-Type: application/x-www-form-urlencoded

grant_type=client_credentials&client_id={{client_id}}&client_secret={{client_secret}}

HTTP 200
[Asserts]
jsonpath "$.token_type" == "Bearer"   # OAuth2 response field, not esync RLS concept
jsonpath "$.access_token" exists
[Captures]
access_token: jsonpath "$.access_token"

# ── Step 2: Use token against esync GraphQL ───────────────────────────────
POST {{gql_url}}
Authorization: Bearer {{access_token}}
Content-Type: application/json
{
  "query": "{ list_order(limit: 5) { id status total } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_order" isCollection
