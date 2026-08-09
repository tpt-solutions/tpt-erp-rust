//! # tpt-tenant (scaffold)
//!
//! Guarantees tenant isolation at the database engine level. Planned for Phase 2:
//!
//! - Tenant identification from subdomain / header / JWT claim.
//! - Postgres Row-Level Security (RLS) policy templates.
//! - Connection middleware that issues `SET LOCAL app.tenant_id = ...` per request.
//! - An Axum extractor exposing the resolved tenant context.
//!
//! This crate currently defines the [`TenantId`] and tenant-resolution error used
//! across the workspace.

use thiserror::Error;
use tpt_primitives::Id;

/// Marker for the Tenant entity, used with [`Id`].
#[derive(Debug)]
pub struct Tenant;
impl tpt_primitives::Entity for Tenant {}

/// A strongly-typed tenant identifier.
pub type TenantId = Id<Tenant>;

/// Errors that can occur while resolving the active tenant.
#[derive(Debug, Error)]
pub enum TenantError {
    #[error("no tenant could be resolved from the request")]
    Unresolved,
    #[error("tenant {0} is not authorized for this resource")]
    Forbidden(TenantId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_is_strongly_typed() {
        let id = TenantId::new();
        assert!(!id.as_str().is_empty());
    }
}
