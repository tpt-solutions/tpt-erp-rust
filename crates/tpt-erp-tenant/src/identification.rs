//! Tenant identification strategies.
//!
//! A request may carry its tenant in several places: a subdomain (`acme.app.com`),
//! an explicit header, or a JWT claim. These pure functions extract a [`TenantSlug`]
//! from each source; mapping a slug to a concrete [`TenantId`] (UUID) happens against
//! the database in the server layer.

use crate::TenantId;
use serde::Deserialize;
use std::fmt;
use uuid::Uuid;

/// The user-facing tenant identifier carried in a subdomain/header/claim. This is
/// distinct from the internal [`TenantId`] (a UUID): the slug is what clients send,
/// the id is what the database keys on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct TenantSlug(pub String);

impl fmt::Display for TenantSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TenantSlug {
    /// Validate a slug is non-empty and contains only safe characters
    /// (alphanumerics, `-`, `_`). Returns an error otherwise.
    pub fn validate(&self) -> Result<(), TenantResolutionError> {
        if self.0.is_empty() {
            return Err(TenantResolutionError::InvalidSlug(self.0.clone()));
        }
        if !self
            .0
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(TenantResolutionError::InvalidSlug(self.0.clone()));
        }
        Ok(())
    }

    /// Deterministically map this slug to a stable [`TenantId`] (UUID v5) so tenants can
    /// be identified without a database round-trip. The same slug always yields the same
    /// id, which keeps tenant-scoped storage and isolation reproducible.
    pub fn to_id(&self) -> TenantId {
        let uuid = Uuid::new_v5(&Uuid::NAMESPACE_DNS, self.0.as_bytes());
        TenantId::from_uuid(uuid)
    }
}

/// Errors raised while resolving a tenant from a request.
#[derive(Debug, thiserror::Error)]
pub enum TenantResolutionError {
    #[error("no tenant could be resolved from the request")]
    Unresolved,
    #[error("invalid tenant slug: {0}")]
    InvalidSlug(String),
    #[error("malformed JWT claims JSON: {0}")]
    MalformedClaims(#[from] serde_json::Error),
}

/// Extract the tenant slug from a `Host` header value. The leftmost DNS label is the
/// tenant; a bare host (`localhost`) or an apex domain (`example.com`) yields none so it is
/// never silently treated as a tenant slug.
///
/// - `acme.example.com` -> `acme`
/// - `example.com` -> `None` (apex domain, not a subdomain)
/// - `localhost` -> `None`
pub fn from_subdomain(host: &str) -> Option<TenantSlug> {
    let host = host.split(':').next().unwrap_or(host);
    // Reject apex-domain requests (e.g. `example.com`): a real tenant subdomain must be a
    // sub-label of a multi-label base (e.g. `acme.example.com`), so the host needs at least
    // three labels. Without this, `example.com` was silently treated as the tenant `example`.
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 3 {
        return None;
    }
    let first = labels[0];
    if first.is_empty() {
        None
    } else {
        Some(TenantSlug(first.to_ascii_lowercase()))
    }
}

/// Extract the tenant slug from an explicit header value (e.g. `X-Tenant-Id`).
pub fn from_header(value: &str) -> Option<TenantSlug> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(TenantSlug(trimmed.to_ascii_lowercase()))
    }
}

/// Extract the tenant slug from a JWT's claims JSON object by field name.
pub fn from_jwt_claims(
    claims_json: &str,
    field: &str,
) -> Result<Option<TenantSlug>, TenantResolutionError> {
    let claims: serde_json::Value = serde_json::from_str(claims_json)?;
    match claims.get(field).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(Some(TenantSlug(s.to_ascii_lowercase()))),
        Some(_) => Ok(None),
        None => Ok(None),
    }
}

/// A fully resolved tenant context for the current request/transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantContext {
    pub id: TenantId,
    pub slug: TenantSlug,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomain_extraction() {
        assert_eq!(
            from_subdomain("acme.example.com"),
            Some(TenantSlug("acme".into()))
        );
        assert_eq!(
            from_subdomain("ACME.example.com"),
            Some(TenantSlug("acme".into()))
        );
        assert_eq!(from_subdomain("localhost"), None);
        assert_eq!(from_subdomain(""), None);
    }

    #[test]
    fn apex_domain_is_rejected() {
        // A two-label apex (`example.com`) is not a tenant subdomain.
        assert_eq!(from_subdomain("example.com"), None);
    }

    #[test]
    fn header_extraction() {
        assert_eq!(from_header("  Acme "), Some(TenantSlug("acme".into())));
        assert_eq!(from_header(""), None);
    }

    #[test]
    fn jwt_claim_extraction() {
        let claims = r#"{"sub":"user-1","tenant":"globex"}"#;
        assert_eq!(
            from_jwt_claims(claims, "tenant").unwrap(),
            Some(TenantSlug("globex".into()))
        );
        assert_eq!(from_jwt_claims(claims, "missing").unwrap(), None);
        assert!(from_jwt_claims("{not json", "tenant").is_err());
    }

    #[test]
    fn slug_validation() {
        assert!(TenantSlug("acme-1".into()).validate().is_ok());
        assert!(TenantSlug("".into()).validate().is_err());
        assert!(TenantSlug("bad slug!".into()).validate().is_err());
    }
}
