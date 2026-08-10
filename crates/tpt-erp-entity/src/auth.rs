//! RBAC hook used by the generated Axum router.
//!
//! The `#[derive(TptApi)]` macro generates handlers that call an [`AuthPolicy`]
//! before every operation. Provide your own policy type, or rely on [`AllowAll`]
//! for trusted/internal deployments.

use std::fmt::Debug;

use thiserror::Error;

/// The CRUD operation a handler is about to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// `GET /` — list / search.
    List,
    /// `GET /:id` — fetch one.
    Read,
    /// `POST /` — create.
    Create,
    /// `PUT /:id` — replace.
    Update,
    /// `DELETE /:id` — remove.
    Delete,
}

/// The actor on whose behalf a request is executed.
///
/// In a real deployment this is extracted from the tenant/JWT middleware
/// (see `tpt-erp-tenant::web`) and stashed in request extensions.
#[derive(Debug, Clone, Default)]
pub struct Principal {
    /// Stable subject identifier (e.g. user id).
    pub subject: Option<String>,
    /// Tenant slug the principal is scoped to.
    pub tenant: Option<String>,
    /// Free-form roles, checked by policies.
    pub roles: Vec<String>,
}

/// Error returned when an [`AuthPolicy`] denies an operation.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("not authorized to {0:?}")]
pub struct AuthError(pub Operation);

/// The RBAC hook invoked by the generated router before each operation.
///
/// Implementors decide whether a [`Principal`] may perform an [`Operation`].
pub trait AuthPolicy: Send + Sync + 'static {
    /// Authorize `op` for `principal`. Return [`AuthError`] to deny.
    fn authorize(op: Operation, principal: &Principal) -> Result<(), AuthError>;
}

/// Permissive policy: authorizes every operation. Useful for trusted/internal
/// services and for the quickstart demo.
pub struct AllowAll;

impl AuthPolicy for AllowAll {
    fn authorize(_op: Operation, _principal: &Principal) -> Result<(), AuthError> {
        Ok(())
    }
}
