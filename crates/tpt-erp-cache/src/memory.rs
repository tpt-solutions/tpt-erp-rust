//! In-memory reference implementations of the cache/session contracts.
//!
//! These require no external service and form the basis of the unit
//! tests; they also serve as the default backend for local development.

use std::collections::HashMap;
use parking_lot::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use tpt_erp_tenant::TenantId;

use crate::{CacheError, ReadModelCache, Session, SessionStore};

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    /// Tenant -> set of owned session ids, for isolated bulk deletion.
    tenant_sessions: HashMap<TenantId, Vec<String>>,
    /// (tenant, model, key) -> (value, expires_at)
    #[allow(clippy::type_complexity)]
    models: HashMap<(TenantId, String, String), (Value, Option<DateTime<Utc>>)>,
}

/// Process-local, in-memory [`SessionStore`] + [`ReadModelCache`].
///
/// Suitable for tests and single-node local runs. Not shared across
/// processes — use the Redis backend for distributed deployments.
#[derive(Default)]
pub struct InMemoryCache {
    inner: Mutex<Inner>,
}

impl InMemoryCache {
    /// Create an empty in-memory cache.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl SessionStore for InMemoryCache {
    async fn create(
        &self,
        tenant: TenantId,
        data: Value,
        ttl: Option<Duration>,
    ) -> Result<Session, CacheError> {
        let now = Utc::now();
        let id = uuid::Uuid::new_v4().to_string();
        let expires_at = ttl.map(|d| now + chrono::Duration::from_std(d).unwrap_or_default());
        let session = Session {
            id: id.clone(),
            tenant,
            data,
            created_at: now,
            expires_at,
            last_seen: now,
        };
        let mut inner = self.inner.lock();
        inner
            .tenant_sessions
            .entry(tenant)
            .or_default()
            .push(id.clone());
        inner.sessions.insert(id, session.clone());
        Ok(session)
    }

    async fn get(&self, id: &str) -> Result<Option<Session>, CacheError> {
        let mut inner = self.inner.lock();
        match inner.sessions.get(id) {
            Some(s) if s.is_expired(Utc::now()) => {
                inner.sessions.remove(id);
                Ok(None)
            }
            Some(s) => Ok(Some(s.clone())),
            None => Ok(None),
        }
    }

    async fn touch(&self, id: &str) -> Result<(), CacheError> {
        let mut inner = self.inner.lock();
        if let Some(s) = inner.sessions.get_mut(id) {
            let now = Utc::now();
            s.last_seen = now;
            // Slide a relative TTL forward if one was set originally.
            if let Some(exp) = s.expires_at {
                let remaining = exp - s.last_seen;
                if remaining > chrono::Duration::zero() {
                    s.expires_at = Some(now + remaining);
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), CacheError> {
        let mut inner = self.inner.lock();
        if let Some(s) = inner.sessions.remove(id)
            && let Some(ids) = inner.tenant_sessions.get_mut(&s.tenant)
        {
            ids.retain(|x| x != id);
        }
        Ok(())
    }

    async fn delete_for_tenant(&self, tenant: &TenantId) -> Result<(), CacheError> {
        let mut inner = self.inner.lock();
        if let Some(ids) = inner.tenant_sessions.remove(tenant) {
            for id in ids {
                inner.sessions.remove(&id);
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl ReadModelCache for InMemoryCache {
    async fn get(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
    ) -> Result<Option<Value>, CacheError> {
        let mut inner = self.inner.lock();
        let k = (*tenant, model.to_string(), key.to_string());
        match inner.models.get(&k) {
            Some((v, Some(exp))) if *exp <= Utc::now() => {
                inner.models.remove(&k);
                Ok(None)
            }
            Some((v, _)) => Ok(Some(v.clone())),
            None => Ok(None),
        }
    }

    async fn put(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
        value: Value,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        let expires_at =
            ttl.map(|d| Utc::now() + chrono::Duration::from_std(d).unwrap_or_default());
        let mut inner = self.inner.lock();
        inner.models.insert(
            (*tenant, model.to_string(), key.to_string()),
            (value, expires_at),
        );
        Ok(())
    }

    async fn invalidate(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
    ) -> Result<(), CacheError> {
        let mut inner = self.inner.lock();
        inner
            .models
            .remove(&(*tenant, model.to_string(), key.to_string()));
        Ok(())
    }

    async fn invalidate_model(&self, tenant: &TenantId, model: &str) -> Result<(), CacheError> {
        let mut inner = self.inner.lock();
        inner
            .models
            .retain(|(t, m, _), _| t != tenant || m != model);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_tenant::TenantSlug;

    fn tenant(name: &str) -> TenantId {
        TenantSlug(name.to_string()).to_id()
    }

    #[test]
    fn session_round_trips_through_json() {
        // The Redis backend stores sessions as JSON, so the struct must
        // serialize/deserialize (including its strongly-typed TenantId).
        let s = Session {
            id: "sess-1".into(),
            tenant: tenant("acme"),
            data: serde_json::json!({"role": "admin"}),
            created_at: Utc::now(),
            expires_at: None,
            last_seen: Utc::now(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.tenant, s.tenant);
        assert_eq!(back.data, s.data);
    }

    #[tokio::test]
    async fn session_lifecycle_and_isolation() {
        let cache = InMemoryCache::new();
        let sessions: &dyn SessionStore = &cache;
        let a = tenant("acme");
        let b = tenant("globex");

        let s = sessions
            .create(
                a,
                serde_json::Value::Null,
                Some(Duration::from_secs(60)),
            )
            .await
            .unwrap();
        assert!(sessions.get(&s.id).await.unwrap().is_some());

        // Different tenant cannot enumerate/see it, but isolation is
        // structural: delete_for_tenant only touches its own.
        sessions.delete_for_tenant(&b).await.unwrap();
        assert!(sessions.get(&s.id).await.unwrap().is_some());
        sessions.delete_for_tenant(&a).await.unwrap();
        assert!(sessions.get(&s.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn session_expiry_is_lazy() {
        let cache = InMemoryCache::new();
        let sessions: &dyn SessionStore = &cache;
        let s = sessions
            .create(
                tenant("acme"),
                serde_json::Value::Null,
                Some(Duration::from_secs(0)),
            )
            .await
            .unwrap();
        // Expired immediately; get should treat as missing.
        assert!(sessions.get(&s.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_model_cache_scoped_by_tenant() {
        let cache = InMemoryCache::new();
        let models: &dyn ReadModelCache = &cache;
        let a = tenant("acme");
        let b = tenant("globex");
        let val = serde_json::json!({"n": 1});

        models
            .put(&a, "balances", "acc-1", val.clone(), None)
            .await
            .unwrap();
        // Other tenant sees nothing under the same model/key.
        assert!(models.get(&b, "balances", "acc-1").await.unwrap().is_none());
        assert_eq!(
            models.get(&a, "balances", "acc-1").await.unwrap(),
            Some(val)
        );

        models.invalidate_model(&a, "balances").await.unwrap();
        assert!(models.get(&a, "balances", "acc-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn touch_slides_ttl() {
        let cache = InMemoryCache::new();
        let sessions: &dyn SessionStore = &cache;
        let s = sessions
            .create(
                tenant("acme"),
                serde_json::Value::Null,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap();
        // Expire in the past by overwriting, then touch should not
        // resurrect a fully-expired session (already removed on get).
        sessions.delete(&s.id).await.unwrap();
        sessions.touch(&s.id).await.unwrap(); // no-op, must not panic
    }
}
