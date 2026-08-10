//! Host side of the WIT `erp` interface.
//!
//! The host exposes a *read-only* view of ERP data to plugins. Plugins
//! never get a mutable handle, never see WASI, and never see raw
//! database connections — they only see the [`HostContext`] trait, which
//! the embedding application implements (typically by querying a CQRS
//! read-model or an in-memory projection).
//!
//! [`TptHost`] is the state threaded through every host call, and it
//! implements the WIT-generated [`Erp`](crate::contract::tpt::erp::Erp)
//! host trait.

use crate::contract::tpt::erp::erp::{Error, Host, Money};

/// Application-supplied data the host makes available to plugins.
///
/// Implementors decide *how* ERP data is read (in-memory projection,
/// Postgres read-model, gRPC call to another service, …). The runtime
/// only requires that reads be total: a missing entity or a missing
/// tenant entitlement is reported back to the guest as a contract
/// [`Error`] rather than a trap.
pub trait HostContext: Send + Sync {
    /// Balance of `account`, or `None` if unknown to this tenant.
    fn account_balance(&self, account: &str) -> Option<crate::Money>;

    /// On-hand base-unit quantity for `sku`, or `None` if unknown.
    fn stock_level(&self, sku: &str) -> Option<u64>;

    /// Tenant this plugin invocation runs for (empty if unattributed).
    fn current_tenant(&self) -> String;

    /// Clone the context behind a fresh box (object-safe clone).
    fn clone_box(&self) -> Box<dyn HostContext>;
}

impl Clone for Box<dyn HostContext> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// State threaded through every host call. Wraps the user's
/// [`HostContext`] plus the per-instance memory limiter.
pub struct TptHost {
    /// User read-model backend.
    pub ctx: std::sync::Arc<dyn HostContext>,
    /// Memory limiter applied to the store owning this host.
    pub limits: wasmtime::StoreLimits,
}

impl TptHost {
    /// Build host state from a user context and memory limiter.
    pub fn new(ctx: std::sync::Arc<dyn HostContext>, limits: wasmtime::StoreLimits) -> Self {
        Self { ctx, limits }
    }
}

// Map our portable `Money` type to the WIT-generated `Money` record.
impl From<crate::Money> for Money {
    fn from(m: crate::Money) -> Self {
        Money {
            major: m.major,
            minor: m.minor,
        }
    }
}

impl Host for TptHost {
    fn get_account_balance(&mut self, account: String) -> Result<Money, Error> {
        match self.ctx.account_balance(&account) {
            Some(m) => Ok(m.into()),
            None => Err(Error::NotFound),
        }
    }

    fn get_stock_level(&mut self, item: String) -> Result<u64, Error> {
        match self.ctx.stock_level(&item) {
            Some(q) => Ok(q),
            None => Err(Error::NotFound),
        }
    }

    fn current_tenant(&mut self) -> String {
        self.ctx.current_tenant()
    }
}
