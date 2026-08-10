//! Redis / Dragonfly backend for the cache/session contracts.
//!
//! Enable with the `redis` feature. Keys are namespaced so tenant
//! isolation holds at the storage layer, not just in application code:
//!
//! - `tpt:sess:{id}`             — JSON-encoded [`Session`], with a Redis
//!   TTL mirroring `expires_at`.
//! - `tpt:sess:tenant:{tid}`     — set of session ids owned by a tenant,
//!   used for isolated bulk deletion.
//! - `tpt:rm:{tid}:{model}:{k}`  — JSON read-model value, with TTL.
//!
//! `tpt:rm:{tid}:{model}:*`      — scanned for `invalidate_model`.

use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use serde_json::Value;
use tokio::sync::Mutex;
use tpt_erp_tenant::TenantId;

use crate::{CacheError, ReadModelCache, Session, SessionStore};

fn sess_key(id: &str) -> String {
    format!("tpt:sess:{id}")
}
fn tenant_set_key(tenant: &TenantId) -> String {
    format!("tpt:sess:tenant:{}", tenant.as_str())
}
fn model_key(tenant: &TenantId, model: &str, key: &str) -> String {
    format!("tpt:rm:{}:{}:{}", tenant.as_str(), model, key)
}

/// Redis/Dragonfly-backed [`SessionStore`].
pub struct RedisSessionStore {
    conn: Mutex<MultiplexedConnection>,
}

impl RedisSessionStore {
    /// Connect to a Redis/Dragonfly server (e.g. `redis://localhost`).
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let client = redis::Client::open(url).map_err(|e| CacheError::Backend(e.to_string()))?;
        let conn = client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

#[async_trait]
impl SessionStore for RedisSessionStore {
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
            tenant: tenant.clone(),
            data,
            created_at: now,
            expires_at,
            last_seen: now,
        };
        let json = serde_json::to_string(&session)?;

        let mut conn = self.conn.lock().await;
        if let Some(ttl) = ttl {
            let secs = u64::try_from(ttl.as_secs()).unwrap_or(u64::MAX);
            let _: () = conn
                .set_ex(sess_key(&id), json, secs)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;
        } else {
            let _: () = conn
                .set(sess_key(&id), json)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;
        }
        let _: () = conn
            .sadd(tenant_set_key(&tenant), &id)
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        Ok(session)
    }

    async fn get(&self, id: &str) -> Result<Option<Session>, CacheError> {
        let mut conn = self.conn.lock().await;
        let raw: Option<String> = conn
            .get(sess_key(id))
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(json) => {
                let s: Session = serde_json::from_str(&json)?;
                if s.is_expired(Utc::now()) {
                    let _: () = conn
                        .del(sess_key(id))
                        .await
                        .map_err(|e| CacheError::Backend(e.to_string()))?;
                    Ok(None)
                } else {
                    Ok(Some(s))
                }
            }
        }
    }

    async fn touch(&self, id: &str) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        let raw: Option<String> = conn
            .get(sess_key(id))
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        if let Some(json) = raw {
            if let Ok(mut s) = serde_json::from_str::<Session>(&json) {
                let now = Utc::now();
                s.last_seen = now;
                if let Some(exp) = s.expires_at {
                    let remaining = exp - s.last_seen;
                    if remaining > chrono::Duration::zero() {
                        s.expires_at = Some(now + remaining);
                    }
                }
                let json = serde_json::to_string(&s)?;
                // Preserve any existing TTL by re-applying its remaining
                // lifetime.
                if let Some(d) = s.expires_at {
                    let secs = (d - Utc::now())
                        .to_std()
                        .map(|d| u64::try_from(d.as_secs()).unwrap_or(0))
                        .unwrap_or(0);
                    if secs > 0 {
                        let _: () = conn
                            .set_ex(sess_key(id), json, secs)
                            .await
                            .map_err(|e| CacheError::Backend(e.to_string()))?;
                    }
                } else {
                    let _: () = conn
                        .set(sess_key(id), json)
                        .await
                        .map_err(|e| CacheError::Backend(e.to_string()))?;
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        // Remove from its tenant set before deleting the session key.
        let raw: Option<String> = conn
            .get(sess_key(id))
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        if let Some(json) = raw {
            if let Ok(s) = serde_json::from_str::<Session>(&json) {
                let _: Result<(), redis::RedisError> =
                    conn.srem(tenant_set_key(&s.tenant), id).await;
            }
        }
        let _: Result<(), redis::RedisError> = conn.del(sess_key(id)).await;
        Ok(())
    }

    async fn delete_for_tenant(&self, tenant: &TenantId) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        let ids: Vec<String> = conn
            .smembers(tenant_set_key(tenant))
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        for id in &ids {
            let _: Result<(), redis::RedisError> = conn.del(sess_key(id)).await;
        }
        let _: Result<(), redis::RedisError> = conn.del(tenant_set_key(tenant)).await;
        Ok(())
    }
}

/// Redis/Dragonfly-backed [`ReadModelCache`].
pub struct RedisReadModelCache {
    conn: Mutex<MultiplexedConnection>,
}

impl RedisReadModelCache {
    /// Connect to a Redis/Dragonfly server (e.g. `redis://localhost`).
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let client = redis::Client::open(url).map_err(|e| CacheError::Backend(e.to_string()))?;
        let conn = client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

#[async_trait]
impl ReadModelCache for RedisReadModelCache {
    async fn get(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
    ) -> Result<Option<Value>, CacheError> {
        let mut conn = self.conn.lock().await;
        let raw: Option<String> = conn
            .get(model_key(tenant, model, key))
            .await
            .map_err(|e| CacheError::Backend(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
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
        let json = serde_json::to_string(&value)?;
        let mut conn = self.conn.lock().await;
        if let Some(ttl) = ttl {
            let secs = u64::try_from(ttl.as_secs()).unwrap_or(u64::MAX);
            let _: () = conn
                .set_ex(model_key(tenant, model, key), json, secs)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;
        } else {
            let _: () = conn
                .set(model_key(tenant, model, key), json)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;
        }
        Ok(())
    }

    async fn invalidate(
        &self,
        tenant: &TenantId,
        model: &str,
        key: &str,
    ) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        let _: Result<(), redis::RedisError> = conn.del(model_key(tenant, model, key)).await;
        Ok(())
    }

    async fn invalidate_model(&self, tenant: &TenantId, model: &str) -> Result<(), CacheError> {
        let pattern = format!("tpt:rm:{}:{}:*", tenant.as_str(), model);
        let mut conn = self.conn.lock().await;
        let mut cursor: u64 = 0;
        loop {
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut *conn)
                .await
                .map_err(|e| CacheError::Backend(e.to_string()))?;
            if !keys.is_empty() {
                let _: Result<(), redis::RedisError> = conn.del(keys).await;
            }
            cursor = new_cursor;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }
}
