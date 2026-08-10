//! # tpt-erp-cache
//!
//! Cache and session infrastructure for TPT ERP, backed by Redis or
//! Dragonfly (or an in-memory reference store for tests/local dev).
//!
//! Two contracts are provided:
//!
//! - [`SessionStore`] — tenant-scoped, TTL-aware web sessions.
//! - [`ReadModelCache`] — the CQRS read-model cache: materialized views
//!   are expensive to rebuild from the event log, so they are cached
//!   per `(tenant, model, key)` and invalidated on write.
//!
//! Both are object-safe traits so the rest of the system depends only on
//! the interface, not the backend. The in-memory implementations in
//! [`memory`] are fully tested; the Redis/Dragonfly backend in
//! [`redis_impl`] is enabled with the `redis` feature.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tpt_erp_tenant::TenantId;

pub mod memory;
#[cfg(feature = "redis")]
pub mod redis_impl;

/// Errors raised by cache/session backends.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Value (de)serialization failed.
    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The backend returned a transport/storage error.
    #[error("cache backend error: {0}")]
    Backend(String),
    /// A requested key/session was not present (and not TTL-expired).
    #[error("not found: {0}")]
    NotFound(String),
}

/// A tenant-scoped web session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    /// Opaque session id (UUID v4).
    pub id: String,
    /// Tenant that owns the session.
    pub tenant: TenantId,
    /// Arbitrary caller-defined session payload (claims, cart, …).
    pub data: Value,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// Absolute expiry, if any.
    pub expires_at: Option<DateTime<Utc>>,
    /// Last activity timestamp, used for idle-timeout and sliding TTL.
    pub last_seen: DateTime<Utc>,
}

impl Session {
    /// Whether the session has expired relative to `now`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            Some(exp) => now > exp,
            None => false,
        }
    }
}

/// Tenant-scoped, TTL-aware session storage.
///
/// Implementations must enforce tenant isolation: a session created for
/// one tenant must never be readable by another, and
/// [`SessionStore::delete_for_tenant`] must remove *only* that tenant's
/// sessions.
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    /// Create a new session for `tenant` with the given payload.
    /// `ttl`, when `Some`, sets an absolute expiry from now.
    async fn create(
        &self,
        tenant: TenantId,
        data: Value,
        ttl: Option<Duration>,
    ) -> Result<Session, CacheError>;

    /// Fetch a session, returning `None` if absent or expired.
    async fn get(&self, id: &str) -> Result<Option<Session>, CacheError>;

    /// Refresh a session's `last_seen` (and, if it was created with a TTL,
    /// slide its expiry forward). Missing sessions are a no-op.
    async fn touch(&self, id: &str) -> Result<(), CacheError>;

    /// Delete a single session.
    async fn delete(&self, id: &str) -> Result<(), CacheError>;

    /// Delete every session belonging to `tenant`.
    async fn delete_for_tenant(&self, tenant: &TenantId) -> Result<(), CacheError>;
}

/// Per-tenant CQRS read-model cache.
///
/// Read models are recomputed from the event log; caching them avoids
/// replay on every read. Keys are namespaced by `(tenant, model, key)`
/// so cross-tenant leakage is impossible by construction.
#[async_trait::async_trait]
pub trait ReadModelCache: Send + Sync {
    /// Get a cached read-model value, or `None` if absent/expired.
    async fn get(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
    ) -> Result<Option<Value>, CacheError>;

    /// Store a read-model value, optionally with a TTL from now.
    async fn put(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
        value: Value,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError>;

    /// Invalidate a single `(tenant, model, key)` entry.
    async fn invalidate(&self, tenant: &TenantId, model: &str, key: &str)
    -> Result<(), CacheError>;

    /// Invalidate every entry for a `(tenant, model)`.
    async fn invalidate_model(&self, tenant: &TenantId, model: &str) -> Result<(), CacheError>;
}
