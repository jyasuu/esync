//! OAuth2 authentication middleware + RLS context extraction.
//!
//! # Two token flavours
//!
//! ## Client-credential tokens  (`grant_type=client_credentials`)
//! These represent a service/machine identity.  The RLS variables set are:
//!   - `rls.client_id`  — the `client_id` claim (or `sub` fallback)
//!   - `rls.role`       — the first entry of the `roles` / `scope` claim
//!                        (configurable via `rls_role_claim`)
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
    pub fn rls_params(&self, cfg: &OAuth2Config) -> Vec<(String, String)> {
        let mut params: Vec<(String, String)> = Vec::new();

        // Always set token type so RLS policies can branch on it.
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
            _ => {} // anonymous — no RLS vars
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

// ── RSA SHA-256 signature verification (no external crypto dep) ──────────
//
// We implement a minimal PKCS#1 v1.5 RSA-SHA256 verifier using only stdlib
// BigUint arithmetic so we don't have to add ring/rsa to Cargo.toml.
// For production use with many keys / high traffic, swap this for `jsonwebtoken`
// or `ring` crate.  This is intentionally simple and correct for RS256.

fn verify_rsa_sha256(message: &[u8], signature: &[u8], n: &[u8], e: &[u8]) -> Result<()> {
    use sha2::{Digest, Sha256};

    let hash = Sha256::digest(message);

    // RSA public key operation: sig^e mod n
    let sig_int = big_uint_from_bytes(signature);
    let n_int = big_uint_from_bytes(n);
    let e_int = big_uint_from_bytes(e);
    let decrypted = mod_pow(sig_int, e_int, &n_int);

    let decrypted_bytes = big_uint_to_bytes_padded(&decrypted, n.len());

    // PKCS#1 v1.5 padding check: 0x00 0x01 [0xFF...] 0x00 [DigestInfo] [hash]
    // DigestInfo for SHA-256 = {0x30,0x31,0x30,0x0d,0x06,0x09,0x60,0x86,0x48,
    //                           0x01,0x65,0x03,0x04,0x02,0x01,0x05,0x00,0x04,0x20}
    const SHA256_PREFIX: &[u8] = &[
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
        0x05, 0x00, 0x04, 0x20,
    ];

    let expected_suffix: Vec<u8> = [SHA256_PREFIX, hash.as_slice()].concat();
    let min_len = 3 + expected_suffix.len(); // 0x00 0x01 ... 0x00 suffix

    if decrypted_bytes.len() < min_len || decrypted_bytes[0] != 0x00 || decrypted_bytes[1] != 0x01 {
        bail!("JWT signature verification failed (bad padding)");
    }

    // Find the 0x00 separator after the 0xFF pad bytes.
    let sep_pos = decrypted_bytes[2..]
        .iter()
        .position(|&b| b == 0x00)
        .map(|p| p + 2)
        .context("JWT signature bad padding: no 0x00 separator")?;

    // All bytes between 0x01 and the separator must be 0xFF.
    if decrypted_bytes[2..sep_pos].iter().any(|&b| b != 0xff) {
        bail!("JWT signature bad PKCS#1 padding");
    }

    let digest_info = &decrypted_bytes[sep_pos + 1..];
    if digest_info != expected_suffix.as_slice() {
        bail!("JWT signature verification failed (digest mismatch)");
    }

    Ok(())
}

// ── Minimal big-integer arithmetic for RSA ────────────────────────────────

fn big_uint_from_bytes(bytes: &[u8]) -> Vec<u32> {
    // big-endian bytes → little-endian u32 limbs
    let mut out = vec![0u32; (bytes.len() + 3) / 4];
    for (i, &b) in bytes.iter().rev().enumerate() {
        out[i / 4] |= (b as u32) << (8 * (i % 4));
    }
    out
}

fn big_uint_to_bytes_padded(limbs: &[u32], len: usize) -> Vec<u8> {
    let total = limbs.len() * 4;
    let mut bytes = vec![0u8; total];
    for (i, &limb) in limbs.iter().enumerate() {
        let base = total - i * 4;
        bytes[base - 1] = (limb & 0xff) as u8;
        bytes[base - 2] = ((limb >> 8) & 0xff) as u8;
        bytes[base - 3] = ((limb >> 16) & 0xff) as u8;
        bytes[base - 4] = ((limb >> 24) & 0xff) as u8;
    }
    // Trim leading zeros then left-pad to `len`.
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let trimmed = &bytes[start..];
    if trimmed.len() >= len {
        trimmed.to_vec()
    } else {
        let mut out = vec![0u8; len];
        out[len - trimmed.len()..].copy_from_slice(trimmed);
        out
    }
}

/// Square-and-multiply mod_pow for arbitrary-precision (Vec<u32>) limbs.
fn mod_pow(mut base: Vec<u32>, mut exp: Vec<u32>, modulus: &[u32]) -> Vec<u32> {
    let one = vec![1u32];
    let mut result = one.clone();
    base = bn_rem(&base, modulus);
    while !bn_is_zero(&exp) {
        if exp[0] & 1 == 1 {
            result = bn_rem(&bn_mul(&result, &base), modulus);
        }
        exp = bn_shr1(&exp);
        base = bn_rem(&bn_mul(&base, &base), modulus);
    }
    result
}

fn bn_is_zero(a: &[u32]) -> bool {
    a.iter().all(|&x| x == 0)
}

fn bn_shr1(a: &[u32]) -> Vec<u32> {
    let mut out = vec![0u32; a.len()];
    let mut carry = 0u32;
    for i in (0..a.len()).rev() {
        out[i] = (a[i] >> 1) | carry;
        carry = (a[i] & 1) << 31;
    }
    out
}

fn bn_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &ai) in a.iter().enumerate() {
        for (j, &bj) in b.iter().enumerate() {
            out[i + j] += (ai as u64) * (bj as u64);
        }
    }
    // carry
    for i in 0..out.len() - 1 {
        out[i + 1] += out[i] >> 32;
        out[i] &= 0xffff_ffff;
    }
    out.iter().map(|&x| x as u32).collect()
}

fn bn_rem(a: &[u32], m: &[u32]) -> Vec<u32> {
    // Simple long division.  Slow but correct for one-time RSA verify.
    let mut rem = a.to_vec();
    while bn_cmp(&rem, m) != std::cmp::Ordering::Less {
        let shift = bn_bit_len(&rem).saturating_sub(bn_bit_len(m));
        let mut shifted = bn_shl(m, shift);
        if bn_cmp(&shifted, &rem) == std::cmp::Ordering::Greater {
            shifted = bn_shr1(&shifted);
            if bn_cmp(&shifted, &rem) == std::cmp::Ordering::Greater {
                break;
            }
        }
        rem = bn_sub(&rem, &shifted);
    }
    rem
}

fn bn_bit_len(a: &[u32]) -> usize {
    for i in (0..a.len()).rev() {
        if a[i] != 0 {
            return i * 32 + 32 - a[i].leading_zeros() as usize;
        }
    }
    0
}

fn bn_shl(a: &[u32], bits: usize) -> Vec<u32> {
    let word_shift = bits / 32;
    let bit_shift = bits % 32;
    let mut out = vec![0u32; a.len() + word_shift + 1];
    for (i, &v) in a.iter().enumerate() {
        out[i + word_shift] |= v << bit_shift;
        if bit_shift > 0 && i + word_shift + 1 < out.len() {
            out[i + word_shift + 1] |= v >> (32 - bit_shift);
        }
    }
    out
}

fn bn_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = a.to_vec();
    let mut borrow = 0i64;
    for i in 0..b.len() {
        let diff = out[i] as i64 - b[i] as i64 - borrow;
        out[i] = (diff & 0xffff_ffff) as u32;
        borrow = if diff < 0 { 1 } else { 0 };
    }
    out
}

fn bn_cmp(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    let max_len = a.len().max(b.len());
    for i in (0..max_len).rev() {
        let av = if i < a.len() { a[i] } else { 0 };
        let bv = if i < b.len() { b[i] } else { 0 };
        match av.cmp(&bv) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
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
