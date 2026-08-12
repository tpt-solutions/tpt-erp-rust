//! Real JWT-based authentication for the Axum integration.
//!
//! Unlike the advisory-only tenant resolution in [`crate::web`] (which trusts a
//! client-supplied `X-Tenant-Id` / `Host` / raw JWT *claim*), this module
//! **verifies** the signature of a `Bearer` token before trusting any of its
//! contents. A verified token populates both:
//!
//! - [`tpt_erp_entity::auth::Principal`] (subject + roles) consumed by the
//!   generated router's [`tpt_erp_entity::auth::AuthPolicy`], and
//! - [`crate::identification::TenantContext`] — the tenant is taken from the
//!   *verified* `tenant` claim, never from the raw request, so tenant selection
//!   is gated behind a verified credential rather than a spoofable header.
//!
//! Verification uses HS256 (HMAC-SHA256). For asymmetric keys (RS256/ES256), swap
//! the signing primitive here — the [`JwtConfig`]/`VerifiedClaims` surface is
//! unchanged.

use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tpt_erp_entity::auth::Principal;

use crate::identification::{TenantContext, TenantSlug};

type HmacSha256 = Hmac<Sha256>;

/// Errors returned while verifying a JWT.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum JwtAuthError {
    #[error("missing or malformed Authorization header")]
    MissingCredentials,
    #[error("unsupported token algorithm: {0}")]
    UnsupportedAlg(String),
    #[error("invalid token signature")]
    InvalidSignature,
    #[error("malformed token: {0}")]
    Malformed(String),
    #[error("token expired")]
    Expired,
    #[error("issuer mismatch")]
    IssuerMismatch,
    #[error("audience mismatch")]
    AudienceMismatch,
    #[error("missing tenant claim")]
    MissingTenant,
}

/// Configuration for issuing and verifying JWTs.
#[derive(Clone)]
pub struct JwtConfig {
    /// Shared secret used for HS256 signing/verification.
    pub secret: Vec<u8>,
    /// If set, the `iss` claim must equal this value.
    pub issuer: Option<String>,
    /// If set, the `aud` claim must include this value.
    pub audience: Option<String>,
    /// Claim key that carries the tenant slug (default `"tenant"`).
    pub tenant_claim: String,
    /// Claim key that carries the role list (default `"roles"`).
    pub roles_claim: String,
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            secret: Vec::new(),
            issuer: None,
            audience: None,
            tenant_claim: "tenant".into(),
            roles_claim: "roles".into(),
        }
    }
}

impl JwtConfig {
    /// Build a config from a shared secret and optional issuer/audience.
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
            ..Default::default()
        }
    }

    /// Set the expected issuer and return `self` (builder style).
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Set the expected audience and return `self` (builder style).
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    /// Mint a signed HS256 token from the given claims.
    pub fn issue(&self, claims: &Claims) -> Result<String, JwtAuthError> {
        let header = serde_json::json!({ "alg": "HS256", "typ": "JWT" });
        let header_b64 = b64url_encode(
            &serde_json::to_vec(&header).map_err(|e| JwtAuthError::Malformed(e.to_string()))?,
        );

        let mut payload = serde_json::Map::new();
        payload.insert("sub".into(), serde_json::Value::String(claims.sub.clone()));
        payload.insert(
            self.tenant_claim.clone(),
            serde_json::Value::String(claims.tenant.clone()),
        );
        payload.insert(
            self.roles_claim.clone(),
            serde_json::to_value(&claims.roles)
                .map_err(|e| JwtAuthError::Malformed(e.to_string()))?,
        );
        if let Some(exp) = claims.exp {
            payload.insert("exp".into(), serde_json::Value::from(exp));
        }
        if let Some(iss) = &claims.iss {
            payload.insert("iss".into(), serde_json::Value::String(iss.clone()));
        }
        if let Some(aud) = &claims.aud {
            payload.insert("aud".into(), serde_json::Value::String(aud.clone()));
        }
        let payload_b64 = b64url_encode(
            &serde_json::to_vec(&serde_json::Value::Object(payload))
                .map_err(|e| JwtAuthError::Malformed(e.to_string()))?,
        );

        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = sign(&signing_input, &self.secret);
        Ok(format!("{signing_input}.{sig}"))
    }

    /// Verify a `Bearer` token string (token only, without the `Bearer ` prefix)
    /// and return the validated claims. Rejects bad signatures, expired tokens,
    /// and issuer/audience mismatches.
    pub fn verify(&self, token: &str) -> Result<VerifiedClaims, JwtAuthError> {
        let (header_b64, payload_b64, sig_b64) = split_token(token)?;

        let header: serde_json::Value = serde_json::from_slice(&b64url_decode(header_b64)?)
            .map_err(|e| JwtAuthError::Malformed(e.to_string()))?;
        let alg = header
            .get("alg")
            .and_then(|v| v.as_str())
            .ok_or_else(|| JwtAuthError::Malformed("missing alg".into()))?;
        if alg != "HS256" {
            return Err(JwtAuthError::UnsupportedAlg(alg.into()));
        }

        let signing_input = format!("{header_b64}.{payload_b64}");
        let expected = sign(&signing_input, &self.secret);
        if !ct_eq(expected.as_bytes(), sig_b64.as_bytes()) {
            return Err(JwtAuthError::InvalidSignature);
        }

        let payload: serde_json::Value = serde_json::from_slice(&b64url_decode(payload_b64)?)
            .map_err(|e| JwtAuthError::Malformed(e.to_string()))?;

        if let Some(exp) = payload.get("exp").and_then(|v| v.as_i64())
            && Utc::now().timestamp() >= exp
        {
            return Err(JwtAuthError::Expired);
        }
        if let Some(iss) = &self.issuer
            && payload.get("iss").and_then(|v| v.as_str()) != Some(iss.as_str())
        {
            return Err(JwtAuthError::IssuerMismatch);
        }
        if let Some(aud) = &self.audience
            && !audience_matches(&payload, aud)
        {
            return Err(JwtAuthError::AudienceMismatch);
        }

        let tenant_str = payload
            .get(&self.tenant_claim)
            .and_then(|v| v.as_str())
            .ok_or(JwtAuthError::MissingTenant)?;
        let slug = TenantSlug(tenant_str.to_ascii_lowercase());
        slug.validate()
            .map_err(|e| JwtAuthError::Malformed(e.to_string()))?;

        let subject = payload
            .get("sub")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let roles = match payload.get(&self.roles_claim) {
            Some(serde_json::Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        };

        Ok(VerifiedClaims {
            subject,
            tenant: slug,
            roles,
        })
    }
}

/// The set of claims carried by an issued token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    /// Stable subject identifier (e.g. user id).
    pub sub: String,
    /// Tenant slug the principal is scoped to.
    pub tenant: String,
    /// Free-form roles, checked by policies.
    pub roles: Vec<String>,
    /// Expiry as a unix timestamp; `None` means "never expires" (not recommended).
    pub exp: Option<i64>,
    /// Optional issuer; validated against [`JwtConfig::issuer`] on verify.
    pub iss: Option<String>,
    /// Optional audience; validated against [`JwtConfig::audience`] on verify.
    pub aud: Option<String>,
}

impl Claims {
    /// Build claims with the bare minimum: subject, tenant, roles.
    pub fn new(sub: impl Into<String>, tenant: impl Into<String>, roles: Vec<String>) -> Self {
        Self {
            sub: sub.into(),
            tenant: tenant.into(),
            roles,
            exp: None,
            iss: None,
            aud: None,
        }
    }

    /// Attach an expiry (seconds from now) and return `self` (builder style).
    pub fn with_ttl_secs(mut self, secs: i64) -> Self {
        self.exp = Some(Utc::now().timestamp() + secs);
        self
    }
}

/// Claims that survived verification — safe to trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedClaims {
    pub subject: String,
    pub tenant: TenantSlug,
    pub roles: Vec<String>,
}

impl VerifiedClaims {
    /// Build the [`Principal`] consumed by the generated router's `AuthPolicy`.
    pub fn to_principal(&self) -> Principal {
        Principal {
            subject: Some(self.subject.clone()),
            tenant: Some(self.tenant.to_string()),
            roles: self.roles.clone(),
        }
    }

    /// Build the [`TenantContext`] resolved purely from the verified claim.
    pub fn to_tenant_context(&self) -> TenantContext {
        TenantContext {
            id: self.tenant.to_id(),
            slug: self.tenant.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn b64url_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn b64url_decode(s: &str) -> Result<Vec<u8>, JwtAuthError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| JwtAuthError::Malformed(e.to_string()))
}

fn split_token(token: &str) -> Result<(&str, &str, &str), JwtAuthError> {
    let mut parts = token.split('.');
    let header = parts
        .next()
        .ok_or(JwtAuthError::Malformed("missing header".into()))?;
    let payload = parts
        .next()
        .ok_or(JwtAuthError::Malformed("missing payload".into()))?;
    let sig = parts
        .next()
        .ok_or(JwtAuthError::Malformed("missing signature".into()))?;
    if parts.next().is_some() {
        return Err(JwtAuthError::Malformed("too many segments".into()));
    }
    Ok((header, payload, sig))
}

fn sign(input: &str, secret: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts key lengths of any size");
    mac.update(input.as_bytes());
    b64url_encode(&mac.finalize().into_bytes())
}

/// Constant-time comparison of two byte slices.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn audience_matches(payload: &serde_json::Value, expected: &str) -> bool {
    match payload.get("aud") {
        Some(serde_json::Value::String(s)) => s == expected,
        Some(serde_json::Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(expected)),
        _ => false,
    }
}

#[cfg(feature = "axum")]
mod middleware {
    use super::*;
    use axum::extract::Request;
    use axum::http::{StatusCode, header::AUTHORIZATION};
    use axum::middleware::Next;
    use axum::response::{IntoResponse, Response};
    use std::sync::Arc;

    /// Extract the raw token following a `Bearer ` prefix.
    fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
        let auth = headers.get(AUTHORIZATION)?.to_str().ok()?;
        auth.strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
            .map(|t| t.trim().to_string())
    }

    /// Axum middleware that verifies a `Bearer` token and, on success, stashes the
    /// verified [`Principal`] and [`TenantContext`] in request extensions so
    /// downstream handlers and the generated router's `AuthPolicy` run against a
    /// real, authenticated actor. Returns `401` on any failure.
    pub async fn auth_middleware(
        axum::extract::State(config): axum::extract::State<Arc<JwtConfig>>,
        mut req: Request,
        next: Next,
    ) -> Response {
        let result = (|| {
            let token = bearer_token(req.headers())
                .ok_or_else(|| (StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;
            let claims = config
                .verify(&token)
                .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
            req.extensions_mut().insert(claims.to_principal());
            req.extensions_mut().insert(claims.to_tenant_context());
            Ok::<(), (StatusCode, String)>(())
        })();

        match result {
            Ok(()) => next.run(req).await,
            Err((status, msg)) => (status, msg).into_response(),
        }
    }
}

#[cfg(feature = "axum")]
pub use middleware::auth_middleware;

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> JwtConfig {
        JwtConfig::new("test-secret")
            .with_issuer("tpt")
            .with_audience("api")
    }

    #[test]
    fn roundtrip_issue_and_verify() {
        let cfg = config();
        let claims = Claims::new("user-1", "acme", vec!["admin".into()]).with_ttl_secs(3600);
        let claims = Claims {
            iss: Some("tpt".into()),
            aud: Some("api".into()),
            ..claims
        };
        let token = cfg.issue(&claims).unwrap();
        let verified = cfg.verify(&token).unwrap();
        assert_eq!(verified.subject, "user-1");
        assert_eq!(verified.tenant, TenantSlug("acme".into()));
        assert_eq!(verified.roles, vec!["admin".to_string()]);
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let cfg = config();
        let claims = Claims::new("user-1", "acme", vec![]);
        let mut token = cfg.issue(&claims).unwrap();
        token.push('x'); // corrupt the signature segment
        assert_eq!(cfg.verify(&token), Err(JwtAuthError::InvalidSignature));
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let cfg = config();
        let other = JwtConfig::new("different-secret");
        let token = cfg.issue(&Claims::new("u", "acme", vec![])).unwrap();
        assert_eq!(other.verify(&token), Err(JwtAuthError::InvalidSignature));
    }

    #[test]
    fn expired_token_is_rejected() {
        let cfg = config();
        let claims = Claims::new("u", "acme", vec![]).with_ttl_secs(-10);
        let token = cfg.issue(&claims).unwrap();
        assert_eq!(cfg.verify(&token), Err(JwtAuthError::Expired));
    }

    #[test]
    fn issuer_mismatch_is_rejected() {
        let cfg = config();
        let claims = Claims {
            iss: Some("evil".into()),
            ..Claims::new("u", "acme", vec![])
        };
        let token = cfg.issue(&claims).unwrap();
        assert_eq!(cfg.verify(&token), Err(JwtAuthError::IssuerMismatch));
    }

    #[test]
    fn audience_mismatch_is_rejected() {
        let cfg = config();
        let claims = Claims {
            iss: Some("tpt".into()),
            aud: Some("other".into()),
            ..Claims::new("u", "acme", vec![])
        };
        let token = cfg.issue(&claims).unwrap();
        assert_eq!(cfg.verify(&token), Err(JwtAuthError::AudienceMismatch));
    }

    #[test]
    fn missing_tenant_is_rejected() {
        let cfg = config();
        // Build a token with no tenant claim by issuing a raw payload.
        let header = b64url_encode(b"{\"alg\":\"HS256\",\"typ\":\"JWT\"}");
        let payload =
            b64url_encode(b"{\"sub\":\"u\",\"roles\":[],\"iss\":\"tpt\",\"aud\":\"api\"}");
        let signing_input = format!("{header}.{payload}");
        let sig = sign(&signing_input, &cfg.secret);
        let token = format!("{signing_input}.{sig}");
        assert_eq!(cfg.verify(&token), Err(JwtAuthError::MissingTenant));
    }

    #[test]
    fn unsupported_algorithm_is_rejected() {
        let cfg = config();
        let header = b64url_encode(b"{\"alg\":\"HS512\",\"typ\":\"JWT\"}");
        let payload = b64url_encode(b"{\"sub\":\"u\",\"tenant\":\"acme\",\"roles\":[]}");
        let signing_input = format!("{header}.{payload}");
        let sig = sign(&signing_input, &cfg.secret);
        let token = format!("{signing_input}.{sig}");
        assert_eq!(
            cfg.verify(&token),
            Err(JwtAuthError::UnsupportedAlg("HS512".into()))
        );
    }

    #[test]
    fn malformed_token_is_rejected() {
        let cfg = config();
        assert!(matches!(
            cfg.verify("not-a-jwt"),
            Err(JwtAuthError::Malformed(_))
        ));
        assert!(matches!(cfg.verify("a.b"), Err(JwtAuthError::Malformed(_))));
    }

    #[test]
    fn verified_claims_build_principal_and_context() {
        let cfg = config();
        let claims = Claims {
            iss: Some("tpt".into()),
            aud: Some("api".into()),
            ..Claims::new("user-9", "globex", vec!["staff".into()])
        };
        let token = cfg.issue(&claims).unwrap();
        let claims = cfg.verify(&token).unwrap();
        let principal = claims.to_principal();
        assert_eq!(principal.subject, Some("user-9".into()));
        assert_eq!(principal.tenant, Some("globex".into()));
        assert_eq!(principal.roles, vec!["staff".to_string()]);
        let ctx = claims.to_tenant_context();
        assert_eq!(ctx.slug, TenantSlug("globex".into()));
    }
}
