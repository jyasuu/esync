# 06_token_rejection.hurl
# Verify that invalid tokens are rejected with HTTP 401.
# Each entry uses a different failure mode.

# Entry 1: Expired token
POST {{gql_url}}
Authorization: Bearer {{expired_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 2: Tampered signature (valid structure, wrong sig bytes)
POST {{gql_url}}
Authorization: Bearer {{bad_sig_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 3: Completely malformed — not a JWT at all
POST {{gql_url}}
Authorization: Bearer not-a-jwt
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 4: Only two JWT parts (missing signature segment)
POST {{gql_url}}
Authorization: Bearer eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 5: Wrong audience claim
POST {{gql_url}}
Authorization: Bearer {{wrong_aud_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401

# Entry 6: Wrong issuer claim
POST {{gql_url}}
Authorization: Bearer {{wrong_iss_token}}
Content-Type: application/json
{
  "query": "{ list_product { id } }"
}

HTTP 401
