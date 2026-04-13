# Keycloak Integration Guide

## Keycloak JWT structure

A Keycloak access token has a different shape from Auth0 / Azure AD tokens.
The key differences that affect esync RLS config:

```jsonc
{
  "iss": "https://iam.example.com/realms/service",  // realm URL, not just host
  "sub": "f33d74d4-a5ed-4da0-817c-...",             // user UUID
  "azp": "esync-svc",                               // client ID (NOT client_id)
  "preferred_username": "alice",                     // present for users, absent for service accounts
  "email": "alice@example.com",                     // present for users only
  "session_state": "f853...",                       // present for interactive sessions only

  // Realm roles (all clients in the realm)
  "realm_access": {
    "roles": ["admin", "offline_access", "uma_authorization"]
  },

  // Client roles (scoped to one client)
  "resource_access": {
    "esync-api": {
      "roles": ["data-reader"]
    }
  }
}
```

**Client-credentials tokens** (service accounts) have `azp` but **no**
`preferred_username`, `email`, or `session_state` — esync auto-detects these as
`token_type = "client_credentials"`.

**User tokens** have `preferred_username`, `email`, and `session_state`.

---

## esync.yaml config

```yaml
graphql:
  oauth2:
    validation_mode: jwks

    # Keycloak 17+ (no /auth prefix):
    jwks_uri: "https://iam.example.com/realms/service/protocol/openid-connect/certs"
    # Keycloak ≤ 16:
    # jwks_uri: "https://iam.example.com/auth/realms/service/protocol/openid-connect/certs"

    required_issuer: "https://iam.example.com/realms/service"
    required_audience: "esync-api"   # your resource server client ID, or omit

    # Keycloak-specific settings:
    rls_role_claim_path: "realm_access.roles"   # nested path, not a top-level claim
    azp_as_client_id: true                       # Keycloak uses azp, not client_id

    rls_user_attributes:
      - sub
      - preferred_username
      - email
      - tenant_id   # add via Keycloak User Attribute mapper
```

### Using client roles instead of realm roles

```yaml
# Pick the first role from the esync-api client scope:
rls_role_claim_path: "resource_access.esync-api.roles"
```

---

## Keycloak admin setup

### 1. Create a realm (if needed)
Admin console → Create Realm → name it `service`

### 2. Create a resource server client
- Clients → Create → Client ID: `esync-api`
- Client Protocol: `openid-connect`
- Access Type: `bearer-only` (it only validates tokens, doesn't issue them)

### 3. Create a service account client  
- Clients → Create → Client ID: `esync-svc`
- Access Type: `confidential`
- Service Accounts Enabled: ON
- Copy the secret from the Credentials tab

### 4. Assign realm roles to the service account
- Clients → `esync-svc` → Service Account Roles
- Assign realm roles: `admin` (or your custom role)

### 5. Add custom user attributes as token claims
For `tenant_id` in user tokens:
- Clients → `esync-api` → Mappers → Create
  - Mapper Type: `User Attribute`
  - User Attribute: `tenant_id`
  - Token Claim Name: `tenant_id`
  - Claim JSON Type: `String`
  - Add to access token: ON

### 6. Postgres RLS policies

```sql
-- Service account with realm role 'admin' sees everything
CREATE POLICY orders_admin ON orders FOR ALL USING (
  current_setting('rls.token_type', true) = 'client_credentials'
  AND current_setting('rls.role', true) = 'admin'
);

-- Authenticated users see only their own orders
CREATE POLICY orders_user ON orders FOR SELECT USING (
  current_setting('rls.token_type', true) = 'user'
  AND user_id::text = current_setting('rls.sub', true)
);
```

---

## Running the Hurl tests

```bash
# Client-credentials (service account)
hurl --variable "keycloak_url=https://iam.example.com" \
     --variable "realm=service" \
     --variable "client_id=esync-svc" \
     --variable "client_secret=<secret>" \
     --variable "gql_url=http://localhost:4000/graphql" \
     examples/keycloak/keycloak_cc.hurl

# User token (requires Direct Access Grants enabled on client)
hurl --variable "keycloak_url=https://iam.example.com" \
     --variable "realm=service" \
     --variable "client_id=esync-frontend" \
     --variable "client_secret=<secret>" \
     --variable "username=testuser@example.com" \
     --variable "password=<pass>" \
     --variable "gql_url=http://localhost:4000/graphql" \
     examples/keycloak/keycloak_user.hurl
```
