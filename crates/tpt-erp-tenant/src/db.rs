//! Postgres-backed tenant scoping.
//!
//! This module turns the RLS command built by [`crate::rls::set_tenant_command`] into a
//! real database action. For every request whose tenant was resolved by the axum
//! extractor/middleware, [`tenant_db_middleware`] opens a transaction on a shared
//! [`sqlx::PgPool`], runs `SET LOCAL app.tenant_id = '<uuid>'`, and exposes the
//! connection through the [`TenantConnection`] extractor so handlers issue queries under
//! that tenant's Row-Level Security policy. The transaction is committed on a successful
//! response and rolled back otherwise.
//!
//! Wire it after the tenant-resolution middleware and provide the pool as router state:
//!
//! ```ignore
//! let app = router
//!     .layer(axum::middleware::from_fn_with_state(
//!         pool.clone(),
//!         tpt_erp_tenant::tenant_db_middleware,
//!     ));
//! ```

use axum::extract::{FromRequestParts, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::TenantContext;
use crate::rls::set_tenant_command;

/// Errors raised by the Postgres tenant middleware / extractor.
#[derive(Debug, thiserror::Error)]
pub enum TenantDbError {
    #[error("could not acquire a database connection")]
    PoolUnavailable,
    #[error("tenant scoping transaction could not be established")]
    ScopingFailed,
    #[error("no tenant-scoped connection was available for this request")]
    NoConnection,
}

impl IntoResponse for TenantDbError {
    fn into_response(self) -> Response {
        let status = match self {
            TenantDbError::PoolUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            TenantDbError::ScopingFailed => StatusCode::INTERNAL_SERVER_ERROR,
            TenantDbError::NoConnection => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

/// A request-scoped Postgres connection that has already been placed inside a transaction
/// with `SET LOCAL app.tenant_id` applied.
///
/// Clone is cheap (an `Arc`); handlers receive it as an extractor and borrow the inner
/// connection with [`TenantConnection::lock`]. The transaction is finalized by the
/// middleware once the response is produced.
#[derive(Clone)]
pub struct TenantConnection {
    inner: Arc<Mutex<Option<sqlx::pool::PoolConnection<sqlx::Postgres>>>>,
}

impl TenantConnection {
    /// Wrap a connection that already has the tenant transaction established.
    fn new(conn: sqlx::pool::PoolConnection<sqlx::Postgres>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(conn))),
        }
    }

    /// Lock the underlying connection for query execution under the tenant's RLS policy.
    ///
    /// Returns `None` if the connection was already finalized (e.g. after the response was
    /// produced), which should not happen for a normally-scoped request.
    pub async fn lock(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Option<sqlx::pool::PoolConnection<sqlx::Postgres>>> {
        self.inner.lock().await
    }
}

impl<S> FromRequestParts<S> for TenantConnection
where
    S: Send + Sync,
{
    type Rejection = TenantDbError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<TenantConnection>()
            .cloned()
            .ok_or(TenantDbError::NoConnection)
    }
}

/// The Postgres tenant-scoping middleware: open a transaction, apply `SET LOCAL
/// app.tenant_id` for the resolved tenant, stash the connection in request extensions, run
/// the handler, then commit on a 2xx response (rollback otherwise).
///
/// Use with `axum::middleware::from_fn_with_state(pool, tenant_db_middleware)`, chained
/// after the tenant-resolution middleware so [`TenantContext`] is present in extensions.
pub async fn tenant_db_middleware(
    State(pool): State<sqlx::PgPool>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    let ctx = req.extensions().get::<TenantContext>().cloned();

    match ctx {
        Some(ctx) => {
            let mut conn = match pool.acquire().await {
                Ok(c) => c,
                Err(_) => return TenantDbError::PoolUnavailable.into_response(),
            };
            // `SET LOCAL` is only valid inside a transaction block.
            if let Err(_) = sqlx::query("BEGIN").execute(&mut *conn).await {
                return TenantDbError::ScopingFailed.into_response();
            }
            let cmd = set_tenant_command(&ctx.id);
            if let Err(_) = sqlx::query(&cmd).execute(&mut *conn).await {
                return TenantDbError::ScopingFailed.into_response();
            }
            req.extensions_mut().insert(TenantConnection::new(conn));
            // Keep our own handle to the scoped connection so we can finalize the
            // transaction after the handler runs (the `Request` is consumed by `next.run`).
            let scoped = req
                .extensions()
                .get::<TenantConnection>()
                .cloned()
                .expect("connection was just inserted");

            let resp = next.run(req).await;

            // Finalize the transaction now that the handler has produced a response.
            if let Some(mut conn) = scoped.inner.lock().await.take() {
                let result = if resp.status().is_success() {
                    sqlx::query("COMMIT").execute(&mut *conn).await
                } else {
                    sqlx::query("ROLLBACK").execute(&mut *conn).await
                };
                if result.is_err() {
                    // Best-effort: ensure the transaction is closed.
                    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                }
            }
            resp
        }
        None => next.run(req).await,
    }
}
