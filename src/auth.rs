//! OAuth2 authentication middleware.
//!
//! After validation, the full JWT payload is serialised as compact JSON and
//! stored in the Postgres GUC `request.jwt.claims` via `SET LOCAL`.  RLS
//! policies use standard `::jsonb` operators to query whatever they need —
//! no esync config changes required when policies evolve.
//!
//! This is PostgREST-compatible: policies written for PostgREST work unchanged.
//!
//! # Validation modes
//!
//! | mode         | description                                           |
//! |---|---|
//! | `jwks`       | Validate RS256 JWT via remote JWKS (default)          |
//! | `introspect` | RFC 7662 token introspection (for opaque tokens)      |
//! | `none`       | Skip validation — dev/test only                       |
//!
//! # Example RLS policies
//!
//! ```sql
//! -- Any top-level claim
//! current_setting('request.jwt.claims', true)::jsonb ->> 'sub'
//! current_setting('request.jwt.claims', true)::jsonb ->> 'azp'
//!
//! -- Keycloak nested realm roles
//! current_setting('request.jwt.claims', true)::jsonb -> 'realm_access' -> 'roles' ? 'admin'
//!
//! -- Custom claim
//! current_setting('request.jwt.claims', true)::jsonb ->> 'tenant_id'
//! ```

use crate::config::{OAuth2Config, ValidationMode};
use anyhow::{bail, Context, Result};
use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::Client;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tracing::{debug, warn};

// ── Public claim context ──────────────────────────────────────────────────

/// The validated JWT payload attached to every GraphQL request.
///
/// `raw_claims` is the compact JSON string that gets stored in
/// `request.jwt.claims` (or the configured GUC parameter name).
/// Postgres policies extract whatever they need via `::jsonb` operators.
#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    /// Full JWT payload as compact JSON, or `"{}"` for anonymous requests.
    pub raw_claims: String,
}

impl AuthContext {
    /// Returns an anonymous context (no token present).
    /// `request.jwt.claims` will be set to `{}` so policies can safely call
    /// `current_setting('request.jwt.claims', true)::jsonb` without erroring.
    pub fn anonymous() -> Self {
        Self {
            raw_claims: "{}".into(),
        }
    }

    /// Build the `SET LOCAL` statements to inject before each Postgres query.
    ///
    /// Returns an empty vec when OAuth2 is not configured — the fast-path runs
    /// queries without a transaction wrapper.  When OAuth2 is active, always
    /// returns at least one entry so the transaction path always executes and
    /// the GUC is set before the query (including for anonymous requests, so
    /// policies that check `request.jwt.claims` never see an unset GUC).
    pub fn rls_params(&self, cfg: &OAuth2Config) -> Vec<(String, String)> {
        if cfg.jwt_claims_param.is_empty() {
            return Vec::new();
        }
        vec![(cfg.jwt_claims_param.clone(), self.raw_claims.clone())]
    }
}

// ── JWKS cache ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CachedKey {
    n: Vec<u8>,
    e: Vec<u8>,
    expires_at: SystemTime,
}

#[derive(Debug, Default)]
struct JwksCache {
    keys: HashMap<String, CachedKey>,
}

// ── Validator ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TokenValidator {
    cfg: Arc<OAuth2Config>,
    http: Client,
    jwks_cache: Arc<RwLock<JwksCache>>,
}

impl TokenValidator {
    pub fn new(cfg: Arc<OAuth2Config>) -> Self {
        Self {
            cfg,
            http: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("HTTP client"),
            jwks_cache: Arc::new(RwLock::new(JwksCache::default())),
        }
    }

    /// Validate a raw Bearer token string and return an `AuthContext`.
    pub async fn validate(&self, raw_token: &str) -> Result<AuthContext> {
        match self.cfg.validation_mode {
            ValidationMode::None => {
                warn!("OAuth2 validation disabled (validation_mode: none) — accepting all tokens");
                self.decode_claims_unverified(raw_token)
            }
            ValidationMode::Introspect => self.introspect(raw_token).await,
            ValidationMode::Jwks => self.validate_jwt(raw_token).await,
        }
    }

    // ── JWT / JWKS validation ──────────────────────────────────────────────

    async fn validate_jwt(&self, token: &str) -> Result<AuthContext> {
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        if parts.len() != 3 {
            bail!("Malformed JWT: expected 3 dot-separated parts");
        }

        let header_bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .context("JWT header base64 decode")?;
        let header: Value =
            serde_json::from_slice(&header_bytes).context("JWT header JSON parse")?;

        let kid = header
            .get("kid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let key = self.get_jwks_key(&kid).await?;

        let message = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .context("JWT signature base64 decode")?;
        verify_rsa_sha256(message.as_bytes(), &signature, &key.n, &key.e)?;

        let payload_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .context("JWT payload base64 decode")?;
        let claims: Value =
            serde_json::from_slice(&payload_bytes).context("JWT payload JSON parse")?;

        self.validate_standard_claims(&claims)?;

        Ok(Self::claims_to_context(&claims))
    }

    async fn get_jwks_key(&self, kid: &str) -> Result<CachedKey> {
        // Fast path: key in cache and not expired.
        {
            let cache = self.jwks_cache.read().unwrap();
            if let Some(k) = cache.keys.get(kid) {
                if k.expires_at > SystemTime::now() {
                    return Ok(k.clone());
                }
            }
        }

        // Slow path: fetch fresh JWKS.
        let jwks_uri = self
            .cfg
            .jwks_uri
            .as_deref()
            .context("jwks_uri required when validation_mode: jwks")?;

        debug!(jwks_uri, "Fetching JWKS");
        let resp: Value = self
            .http
            .get(jwks_uri)
            .send()
            .await
            .context("JWKS fetch")?
            .json()
            .await
            .context("JWKS JSON parse")?;

        let keys = resp["keys"]
            .as_array()
            .context("JWKS response missing 'keys' array")?;

        let mut cache = self.jwks_cache.write().unwrap();
        let expires_at =
            SystemTime::now() + Duration::from_secs(self.cfg.jwks_cache_ttl_secs.unwrap_or(300));

        // Replace the entire cache on each refresh so rotated-away keys are
        // removed immediately and cannot be used after their kid disappears.
        cache.keys.clear();

        for key_obj in keys {
            let k = key_obj.get("kid").and_then(Value::as_str).unwrap_or("");
            let n_b64 = key_obj.get("n").and_then(Value::as_str).unwrap_or("");
            let e_b64 = key_obj.get("e").and_then(Value::as_str).unwrap_or("");

            if let (Ok(n), Ok(e)) = (URL_SAFE_NO_PAD.decode(n_b64), URL_SAFE_NO_PAD.decode(e_b64)) {
                cache
                    .keys
                    .insert(k.to_string(), CachedKey { n, e, expires_at });
            }
        }

        cache
            .keys
            .get(kid)
            .cloned()
            .with_context(|| format!("kid '{kid}' not found in JWKS"))
    }

    fn validate_standard_claims(&self, claims: &Value) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        if let Some(exp) = claims.get("exp").and_then(Value::as_i64) {
            if now > exp {
                bail!("Token expired");
            }
        }
        if let Some(nbf) = claims.get("nbf").and_then(Value::as_i64) {
            let skew = self.cfg.clock_skew_secs.unwrap_or(30) as i64;
            if now < nbf - skew {
                bail!("Token not yet valid (nbf)");
            }
        }
        if let Some(ref required_iss) = self.cfg.required_issuer {
            let iss = claims.get("iss").and_then(Value::as_str).unwrap_or("");
            if iss != required_iss {
                bail!("Issuer mismatch: got '{iss}', expected '{required_iss}'");
            }
        }
        if let Some(ref required_aud) = self.cfg.required_audience {
            let aud_ok = match claims.get("aud") {
                Some(Value::String(s)) => s == required_aud,
                Some(Value::Array(arr)) => arr
                    .iter()
                    .any(|a| a.as_str() == Some(required_aud.as_str())),
                _ => false,
            };
            if !aud_ok {
                bail!("Audience mismatch");
            }
        }
        Ok(())
    }

    // ── Introspection (RFC 7662) ───────────────────────────────────────────

    async fn introspect(&self, token: &str) -> Result<AuthContext> {
        let endpoint = self
            .cfg
            .introspect_endpoint
            .as_deref()
            .context("introspect_endpoint required when validation_mode: introspect")?;

        let mut form = HashMap::new();
        form.insert("token", token);

        let mut req = self.http.post(endpoint).form(&form);

        if let (Some(id), Some(secret)) = (
            self.cfg.client_id.as_deref(),
            self.cfg.client_secret.as_deref(),
        ) {
            req = req.basic_auth(id, Some(secret));
        }

        let resp: Value = req
            .send()
            .await
            .context("Token introspection request")?
            .json()
            .await
            .context("Token introspection JSON parse")?;

        if resp.get("active").and_then(Value::as_bool) != Some(true) {
            bail!("Token is not active (introspection)");
        }

        Ok(Self::claims_to_context(&resp))
    }

    // ── Claim extraction ──────────────────────────────────────────────────

    fn decode_claims_unverified(&self, token: &str) -> Result<AuthContext> {
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        if parts.len() < 2 {
            bail!("Malformed JWT");
        }
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .context("JWT payload base64")?;
        let claims: Value = serde_json::from_slice(&payload_bytes).context("JWT payload JSON")?;
        Ok(Self::claims_to_context(&claims))
    }

    /// Serialise the full claims object into an `AuthContext`.
    /// No claim interpretation — the raw JSON goes straight to Postgres.
    fn claims_to_context(claims: &Value) -> AuthContext {
        AuthContext {
            raw_claims: serde_json::to_string(claims).unwrap_or_else(|_| "{}".into()),
        }
    }
}

// ── RSA-SHA256 signature verification via `rsa` crate ────────────────────

fn verify_rsa_sha256(message: &[u8], signature: &[u8], n: &[u8], e: &[u8]) -> Result<()> {
    use rsa::{
        pkcs1v15::{Signature, VerifyingKey},
        signature::Verifier,
        BigUint, RsaPublicKey,
    };
    use sha2::Sha256;

    let public_key = RsaPublicKey::new(BigUint::from_bytes_be(n), BigUint::from_bytes_be(e))
        .context("Failed to construct RSA public key from JWKS n/e")?;

    let verifying_key = VerifyingKey::<Sha256>::new(public_key);
    let sig = Signature::try_from(signature).context("Failed to parse RSA signature bytes")?;

    verifying_key
        .verify(message, &sig)
        .context("JWT signature verification failed")?;

    Ok(())
}

// ── Axum extractor ────────────────────────────────────────────────────────

/// Extracts the Bearer token from the `Authorization` header and validates it.
/// Returns `AuthContext::anonymous()` when no token is present and
/// `require_auth` is false, or when OAuth2 is not configured at all.
pub struct ExtractAuth(pub AuthContext);

impl<S> FromRequestParts<S> for ExtractAuth
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, String);

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = std::result::Result<Self, Self::Rejection>> + Send {
        let validator = parts.extensions.get::<Arc<TokenValidator>>().cloned();

        let auth_header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        async move {
            let validator = match validator {
                Some(v) => v,
                None => return Ok(ExtractAuth(AuthContext::anonymous())),
            };

            let auth_header = auth_header.unwrap_or_default();

            if auth_header.is_empty() {
                if validator.cfg.require_auth {
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        "Authorization header required".into(),
                    ));
                }
                return Ok(ExtractAuth(AuthContext::anonymous()));
            }

            let token = auth_header
                .strip_prefix("Bearer ")
                .unwrap_or(&auth_header)
                .trim()
                .to_owned();

            match validator.validate(&token).await {
                Ok(ctx) => Ok(ExtractAuth(ctx)),
                Err(e) => {
                    warn!("Token validation failed: {e}");
                    Err((StatusCode::UNAUTHORIZED, format!("Invalid token: {e}")))
                }
            }
        }
    }
}
