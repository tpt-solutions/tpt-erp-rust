//! Axum integration for tenant resolution (enabled by the `axum` feature).
//!
//! - [`TenantContext`] is an Axum extractor: any handler parameter `TenantContext` will
//!   be populated from the request, resolving the tenant from a `Host` subdomain or the
//!   `X-Tenant-Id` header (in that priority order).
//! - [`tenant_context_middleware`] is an optional middleware that pre-resolves the tenant
//!   and stashes it in request extensions; this is also where a live database connection
//!   would issue `SET LOCAL app.tenant_id = '<uuid>'` per transaction.

use crate::identification::{TenantSlug, from_header, from_subdomain};
use crate::{TenantContext, TenantResolutionError};
use axum::extract::FromRequestParts;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::http::header::HOST;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Header used to pass an explicit tenant slug.
pub const TENANT_HEADER: &str = "x-tenant-id";

impl<S> FromRequestParts<S> for TenantContext
where
    S: Send + Sync,
{
    type Rejection = TenantResolutionError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Fast path: a previous layer already resolved the tenant.
        if let Some(ctx) = parts.extensions.get::<TenantContext>() {
            return Ok(ctx.clone());
        }

        let slug = resolve_slug(&parts.headers).ok_or(TenantResolutionError::Unresolved)?;
        slug.validate()?;
        Ok(TenantContext {
            id: slug.to_id(),
            slug,
        })
    }
}

impl IntoResponse for TenantResolutionError {
    fn into_response(self) -> Response {
        (axum::http::StatusCode::BAD_REQUEST, self.to_string()).into_response()
    }
}

/// Resolve a tenant slug from the request headers (Host subdomain first, then header).
fn resolve_slug(headers: &HeaderMap) -> Option<TenantSlug> {
    if let Some(host) = headers.get(HOST).and_then(|v| v.to_str().ok())
        && let Some(slug) = from_subdomain(host)
    {
        return Some(slug);
    }
    headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(from_header)
}

/// Middleware that resolves the tenant once and records it in request extensions.
///
/// This is the connection-agnostic variant (no database needed): it simply stashes the
/// resolved [`TenantContext`] so downstream handlers can extract it. When a real Postgres
/// connection is available, prefer [`tenant_rls_middleware`] (enabled by the `sqlx`
/// feature), which additionally executes `SET LOCAL app.tenant_id` so Row-Level Security
/// actually scopes every query for the request.
pub async fn tenant_context_middleware(req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    if let Some(slug) = resolve_slug(&parts.headers)
        && slug.validate().is_ok()
    {
        let ctx = TenantContext {
            id: slug.to_id(),
            slug: slug.clone(),
        };
        parts.extensions.insert(ctx);
    }
    let req = Request::from_parts(parts, body);
    next.run(req).await
}

/// A per-request Postgres connection that runs every query inside a tenant-scoped
/// transaction: `SET LOCAL app.tenant_id = $1` is executed on `begin()`, so the
/// database's Row-Level Security policies transparently isolate the tenant's rows.
///
/// Handlers obtain this via the [`TenantDb`] extractor and run their queries through
/// [`TenantDb::with_rls`]; the transaction is committed automatically once the closure
/// returns.
#[cfg(feature = "sqlx")]
pub struct TenantDb {
    tenant: String,
    conn: tokio::sync::Mutex<sqlx::pool::PoolConnection<sqlx::Postgres>>,
}

#[cfg(feature = "sqlx")]
impl TenantDb {
    /// Run `f` inside a tenant-scoped transaction. The `SET LOCAL` command is applied on
    /// `begin()` (parameterized, so there is no SQL-injection surface), and the
    /// transaction is committed before returning. Any error from `f` is propagated and
    /// the transaction rolled back.
    pub async fn with_rls<F, Fut, R>(&self, f: F) -> Result<R, sqlx::Error>
    where
        F: FnOnce(&mut sqlx::PgConnection) -> Fut,
        Fut: core::future::Future<Output = Result<R, sqlx::Error>>,
    {
        let mut conn = self.conn.lock().await;
        let mut tx = sqlx::Acquire::begin(&mut *conn).await?;
        // Wire RLS for real: this is the command that used to be built and discarded.
        // `SET LOCAL` only lives for the transaction, so every query below runs on the
        // same connection and is transparently scoped by Postgres Row-Level Security.
        sqlx::query("SET LOCAL app.tenant_id = $1")
            .bind(&self.tenant)
            .execute(&mut *tx)
            .await?;
        let result = f(&mut tx).await;
        match result {
            Ok(r) => {
                tx.commit().await?;
                Ok(r)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}

/// Middleware (enabled by the `sqlx` feature) that resolves the tenant and, when a
/// Postgres connection is available, opens a connection and exposes it via [`TenantDb`]
/// so every handler query is RLS-scoped to `app.tenant_id`.
///
/// Handlers retrieve the scoped connection with `axum::extract::Extension<Arc<TenantDb>>`
/// and run their queries through [`TenantDb::with_rls`]. The router must provide the pool
/// as its state, e.g.
/// `Router::new().layer(from_fn(tenant_rls_middleware)).with_state(pool)`.
#[cfg(feature = "sqlx")]
pub async fn tenant_rls_middleware(
    axum::extract::State(pool): axum::extract::State<sqlx::PgPool>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(slug) = resolve_slug(req.headers())
        && slug.validate().is_ok()
    {
        let ctx = TenantContext {
            id: slug.to_id(),
            slug: slug.clone(),
        };
        req.extensions_mut().insert(ctx.clone());
        if let Ok(conn) = pool.acquire().await {
            let db = std::sync::Arc::new(TenantDb {
                tenant: ctx.id.as_str().to_string(),
                conn: tokio::sync::Mutex::new(conn),
            });
            req.extensions_mut().insert(db);
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/whoami",
                get(|TenantContext { id, slug }: TenantContext| async move {
                    format!("{slug}:{id}")
                }),
            )
            .layer(axum::middleware::from_fn(tenant_context_middleware))
    }

    #[tokio::test]
    async fn extractor_resolves_from_header() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .header(TENANT_HEADER, "acme")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.starts_with("acme:"));
    }

    #[tokio::test]
    async fn extractor_resolves_from_subdomain() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .header(HOST, "globex.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.starts_with("globex:"));
    }

    #[tokio::test]
    async fn missing_tenant_is_rejected() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
