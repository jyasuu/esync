# keycloak_user.hurl
# Resource-owner password flow for integration tests against a real Keycloak realm.
# (Use only in test environments — password grant is disabled by default in Keycloak 18+
#  and must be explicitly enabled on the client: Settings → Direct access grants.)
#
# Variables required:
#   keycloak_url    https://iam.example.com
#   realm           service
#   client_id       esync-frontend
#   client_secret   <secret>   (omit or leave empty for public clients)
#   username        testuser@example.com
#   password        <password>
#   gql_url         http://localhost:4000/graphql
#
# Run:
#   hurl --variable "keycloak_url=https://iam.example.com" \
#        --variable "realm=service" \
#        --variable "client_id=esync-frontend" \
#        --variable "client_secret=<secret>" \
#        --variable "username=testuser@example.com" \
#        --variable "password=<pass>" \
#        --variable "gql_url=http://localhost:4000/graphql" \
#        keycloak_user.hurl

# ── Step 1: Fetch user token from Keycloak ───────────────────────────────
POST {{keycloak_url}}/realms/{{realm}}/protocol/openid-connect/token
Content-Type: application/x-www-form-urlencoded

grant_type=password&client_id={{client_id}}&client_secret={{client_secret}}&username={{username}}&password={{password}}&scope=openid

HTTP 200
[Asserts]
jsonpath "$.token_type" == "Bearer"   # OAuth2 response field, not esync RLS concept
jsonpath "$.access_token" exists
[Captures]
access_token: jsonpath "$.access_token"

# ── Step 2: Verify token has expected Keycloak claims ────────────────────
# Keycloak returns the access token as a JWT — decode payload to inspect.
# (Hurl can't decode JWT natively; use this entry as documentation of
#  expected claim structure. Adjust assertions to your realm/client setup.)

# ── Step 3: Use token against esync GraphQL ──────────────────────────────
# User token → request.jwt.claims contains sub, email, and other claims.
# RLS policies query them directly via ::jsonb.
POST {{gql_url}}
Authorization: Bearer {{access_token}}
Content-Type: application/json
{
  "query": "{ list_order(limit: 10) { id status total } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
jsonpath "$.data.list_order" isCollection

# ── Step 4: Confirm token-type claim detection ───────────────────────────
# Query a field that would be empty if RLS was misconfigured.
# Adjust entity/field names to match your esync.yaml.
POST {{gql_url}}
Authorization: Bearer {{access_token}}
Content-Type: application/json
{
  "query": "{ list_order(limit: 1) { id } }"
}

HTTP 200
[Asserts]
jsonpath "$.errors" not exists
