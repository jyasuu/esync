//! OAuth2 authentication middleware + RLS context extraction.
//!
//! # Two token flavours
//!
//! ## Client-credential tokens  (`grant_type=client_credentials`)
//! These represent a service/machine identity.  The RLS variables set are:
//!   - `rls.client_id`  — the `client_id` claim (or `sub` fallback)
//!   - `rls.role`       — the first entry of the `roles` / `scope` claim
//!     (configurable via `rls_role_claim`)
//!   - `rls.token_type` — `"client_credentials"`
//!
//! ## User tokens  (`grant_type=authorization_code` / OIDC / password)
//! These represent an end-user identity.  The RLS variables set are driven by
//! the per-entity `rls_user_attribute` config (e.g. `sub`, `email`, `tenant_id`).
//!   - `rls.<attr>`     — value of each configured claim attribute
//!   - `rls.user_id`    — always set to the `sub` claim
//!   - `rls.token_type` — `"user"`
//!
//! # Validation modes (configured per deployment)
//!
//! | mode          | description                                              |
//! |---------------|----------------------------------------------------------|
//! | `jwks`        | Validate JWT signature via remote JWKS URL (default)    |
//! | `introspect`  | Call RFC 7662 introspection endpoint (opaque tokens)     |
//! | `none`        | Skip validation — dev/test only, never for production    |

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

// ── Public claim context ─────────────────────────────────────────────────

/// Decoded + validated claims from a token.  Attached to every GraphQL request.
#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    /// `"client_credentials"` or `"user"` or `"anonymous"` (when auth is disabled).
    pub token_type: String,
    /// For client-credential tokens: the `client_id` (or `sub`) claim.
    pub client_id: Option<String>,
    /// The `sub` claim (always present after successful validation).
    pub subject: Option<String>,
    /// The resolved role (from `roles` / `scope` claim).
    pub role: Option<String>,
    /// Raw flat claim map — used for dynamic RLS attribute injection.
    pub claims: HashMap<String, String>,
}

impl AuthContext {
    pub fn anonymous() -> Self {
        Self {
            token_type: "anonymous".into(),
            ..Default::default()
        }
    }

    /// Build SET LOCAL statements for PostgreSQL RLS.
    /// Returns a Vec of (parameter_name, value) pairs.
    ///
    /// Always returns at least `[("rls.token_type", "<type>")]` — even for
    /// anonymous requests — so callers can detect "OAuth2 is active" vs
    /// "OAuth2 is disabled" by checking whether this vec is empty.
    /// An empty vec means OAuth2 is not configured (fast-path, no transaction).
    /// A non-empty vec (including anonymous) always goes through the transaction
    /// path so `SET LOCAL rls.token_type` is visible to RLS policies.
    pub fn rls_params(&self, cfg: &OAuth2Config) -> Vec<(String, String)> {
        let mut params: Vec<(String, String)> = Vec::new();

        // Always set token type so RLS policies can branch on it.
        // For anonymous: value is 'anonymous' — the RESTRICTIVE deny-all policy
        // fires because no permissive policy matches 'anonymous'.
        params.push(("rls.token_type".into(), self.token_type.clone()));

        match self.token_type.as_str() {
            "client_credentials" => {
                if let Some(ref cid) = self.client_id {
                    params.push(("rls.client_id".into(), cid.clone()));
                }
                if let Some(ref role) = self.role {
                    params.push(("rls.role".into(), role.clone()));
                }
            }
            "user" => {
                if let Some(ref sub) = self.subject {
                    params.push(("rls.user_id".into(), sub.clone()));
                }
                // Inject every configured user attribute claim.
                for attr in &cfg.rls_user_attributes {
                    if let Some(val) = self.claims.get(attr.as_str()) {
                        let key = format!("rls.{attr}");
                        params.push((key, val.clone()));
                    }
                }
            }
            // "anonymous" and any future unknown type:
            // rls.token_type is already set above; no additional vars needed.
            // The RESTRICTIVE deny-all policy blocks access since no permissive
            // policy matches the 'anonymous' token type.
            _ => {}
        }

        params
    }
}

// ── JWKS cache ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CachedKey {
    n: Vec<u8>,
    e: Vec<u8>,
    expires_at: SystemTime,
}

#[derive(Debug, Default)]
struct JwksCache {
    keys: HashMap<String, CachedKey>, // kid → key
}

// ── Validator ────────────────────────────────────────────────────────────

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

    /// Validate a raw Bearer token string → `AuthContext`.
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

    // ── JWT / JWKS validation ─────────────────────────────────────────────

    async fn validate_jwt(&self, token: &str) -> Result<AuthContext> {
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        if parts.len() != 3 {
            bail!("Malformed JWT: expected 3 dot-separated parts");
        }

        // Decode header to get kid + alg (we only support RS256/RS384/RS512).
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

        // Load / refresh JWKS.
        let key = self.get_jwks_key(&kid).await?;

        // Verify signature.
        let message = format!("{}.{}", parts[0], parts[1]);
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .context("JWT signature base64 decode")?;
        verify_rsa_sha256(message.as_bytes(), &signature, &key.n, &key.e)?;

        // Decode payload.
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .context("JWT payload base64 decode")?;
        let claims: Value =
            serde_json::from_slice(&payload_bytes).context("JWT payload JSON parse")?;

        // Validate standard claims.
        self.validate_standard_claims(&claims)?;

        Ok(self.build_auth_context(&claims))
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

        // Slow path: fetch JWKS.
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

    // ── Introspection (RFC 7662) ──────────────────────────────────────────

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

        Ok(self.build_auth_context(&resp))
    }

    // ── Claim extraction ─────────────────────────────────────────────────

    fn decode_claims_unverified(&self, token: &str) -> Result<AuthContext> {
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        if parts.len() < 2 {
            bail!("Malformed JWT");
        }
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .context("JWT payload base64")?;
        let claims: Value = serde_json::from_slice(&payload_bytes).context("JWT payload JSON")?;
        Ok(self.build_auth_context(&claims))
    }

    fn build_auth_context(&self, claims: &Value) -> AuthContext {
        let sub = claims.get("sub").and_then(Value::as_str).map(str::to_owned);

        // Detect token type.
        let grant_type = claims
            .get("grant_type")
            .or_else(|| claims.get("gty"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let is_client_cred = grant_type.contains("client_credentials")
            || match &self.cfg.token_type_claim {
                Some(claim) => claims
                    .get(claim.as_str())
                    .and_then(Value::as_str)
                    .map(|v| v == "client_credentials")
                    .unwrap_or(false),
                None => false,
            }
            || (claims.get("client_id").is_some()
                && claims.get("username").is_none()
                && claims.get("preferred_username").is_none()
                && claims.get("email").is_none());

        // Flatten all scalar claims into a string map.
        let mut flat: HashMap<String, String> = HashMap::new();
        if let Some(obj) = claims.as_object() {
            for (k, v) in obj {
                let s = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Array(arr) => arr
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => continue,
                };
                flat.insert(k.clone(), s);
            }
        }

        if is_client_cred {
            let client_id = claims
                .get("client_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| sub.clone());

            // Role: check configured claim, then fall back to first scope word.
            let role_claim = self.cfg.rls_role_claim.as_deref().unwrap_or("roles");
            let role = claims
                .get(role_claim)
                .and_then(|v| match v {
                    Value::String(s) => Some(s.split_whitespace().next()?.to_owned()),
                    Value::Array(arr) => arr.first().and_then(Value::as_str).map(str::to_owned),
                    _ => None,
                })
                .or_else(|| {
                    claims
                        .get("scope")
                        .and_then(Value::as_str)
                        .and_then(|s| s.split_whitespace().next().map(str::to_owned))
                });

            AuthContext {
                token_type: "client_credentials".into(),
                client_id,
                subject: sub,
                role,
                claims: flat,
            }
        } else {
            AuthContext {
                token_type: "user".into(),
                client_id: None,
                subject: sub,
                role: None,
                claims: flat,
            }
        }
    }
}

// ── RSA-SHA256 signature verification via `rsa` crate ───────────────────
//
// Uses rsa 0.9 + sha2 0.10 for PKCS#1 v1.5 verification.
// The hand-rolled bignum implementation had overflow panics in bn_mul
// (u64 accumulator overflow for 2048-bit keys) and a shift-by-32 panic
// in bn_shl when bit_shift == 0.  The `rsa` crate handles all of this
// correctly with a proven big-integer backend (num-bigint).

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

// ── Axum extractor ───────────────────────────────────────────────────────

/// Extracts the Bearer token from `Authorization` header and validates it.
/// If OAuth2 is disabled in config, returns `AuthContext::anonymous()`.
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
        // Snapshot everything we need from `parts` before entering the async block
        // so the future doesn't borrow `parts` across an await point.
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
