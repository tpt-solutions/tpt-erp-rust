//! # tpt-erp-tenant
//!
//! Guarantees tenant isolation at the database engine level.
//!
//! - [`identification`] — extract the active tenant from subdomain / header / JWT claim.
//! - [`rls`] — Postgres Row-Level Security policy templates and the `SET LOCAL`
//!   command that scopes each transaction to a tenant.
//!
//! The Axum extractor/middleware that ties these to a request is added in a later step;
//! the primitives here are pure and fully unit-tested.

use thiserror::Error;
use tpt_erp_primitives::Id;

/// Marker for the Tenant entity, used with [`Id`].
#[derive(Debug)]
pub struct Tenant;
impl tpt_erp_primitives::Entity for Tenant {}

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

pub mod identification;
pub mod rls;

#[cfg(feature = "axum")]
pub mod web;

pub use identification::{
    TenantContext, TenantResolutionError, TenantSlug, from_header, from_jwt_claims, from_subdomain,
};
pub use rls::{TENANT_GUC, disable_rls, enable_rls, rls_policy, set_tenant_command};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_is_strongly_typed() {
        let id = TenantId::new();
        assert!(!id.as_str().is_empty());
    }
}
