//! Axum integration for tenant resolution (enabled by the `axum` feature).
//!
//! - [`TenantContext`] is an Axum extractor: any handler parameter `TenantContext` will
//!   be populated from the request, resolving the tenant from a `Host` subdomain or the
//!   `X-Tenant-Id` header (in that priority order). Both are unauthenticated hints; a
//!   verified tenant from [`crate::auth::auth_middleware`] always takes precedence.
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

/// Request-extension marker inserted by [`tenant_context_middleware`] /
/// [`tenant_rls_middleware`] when the tenant came from unauthenticated Host/header hints.
/// Its presence is what lets the extractor fall back to those hints; without it the
/// extractor only accepts a verified [`TenantContext`] from `auth_middleware`.
#[derive(Debug, Clone, Copy)]
pub struct UnverifiedTenantHints;

impl<S> FromRequestParts<S> for TenantContext
where
    S: Send + Sync,
{
    type Rejection = TenantResolutionError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Fast path: a previous layer already resolved the tenant (verified or advisory).
        if let Some(ctx) = parts.extensions.get::<TenantContext>() {
            return Ok(ctx.clone());
        }

        // Fail closed: unauthenticated Host/header hints are only honoured on routes that
        // explicitly opted in via `tenant_context_middleware`.
        if parts.extensions.get::<UnverifiedTenantHints>().is_none() {
            return Err(TenantResolutionError::Unresolved);
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

/// Resolve a tenant slug from the request headers, in priority order: the `Host`
/// subdomain, then the explicit `X-Tenant-Id` header.
///
/// These sources are **unauthenticated** — any client can send them. They are only
/// suitable behind a trusted proxy or as a routing hint; tenant authorization must come
/// from a verified credential ([`crate::auth::auth_middleware`]). The previous fallback to
/// an unsigned JWT `tenant` claim was removed: it accepted any forged payload.
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
    // A tenant already established by a verifying layer (e.g. `auth_middleware`) is
    // authoritative; never let spoofable Host/header/unsigned-JWT values override it.
    if parts.extensions.get::<TenantContext>().is_none()
        && let Some(slug) = resolve_slug(&parts.headers)
        && slug.validate().is_ok()
    {
        let ctx = TenantContext {
            id: slug.to_id(),
            slug: slug.clone(),
        };
        parts.extensions.insert(ctx);
        parts.extensions.insert(UnverifiedTenantHints);
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
        // Wire RLS for real via the single consolidated, bind-safe query
        // (`set_config('app.tenant_id', $1, true)`). `SET LOCAL` only lives for the
        // transaction, so every query below runs on the same connection and is
        // transparently scoped by Postgres Row-Level Security.
        sqlx::query(crate::rls::SET_TENANT_QUERY)
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
    // Prefer a tenant already verified by an upstream layer; only fall back to
    // header/Host resolution when none is present.
    let ctx = match req.extensions().get::<TenantContext>() {
        Some(ctx) => Some(ctx.clone()),
        None => resolve_slug(req.headers())
            .filter(|slug| slug.validate().is_ok())
            .map(|slug| TenantContext {
                id: slug.to_id(),
                slug,
            }),
    };
    if let Some(ctx) = ctx {
        if req.extensions().get::<TenantContext>().is_none() {
            req.extensions_mut().insert(UnverifiedTenantHints);
        }
        req.extensions_mut().insert(ctx.clone());
        // Fail closed: a resolved tenant without a scoped connection must not proceed.
        match pool.acquire().await {
            Ok(conn) => {
                let db = std::sync::Arc::new(TenantDb {
                    tenant: ctx.id.as_str().to_string(),
                    conn: tokio::sync::Mutex::new(conn),
                });
                req.extensions_mut().insert(db);
            }
            Err(_) => {
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "database unavailable",
                )
                    .into_response();
            }
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

    /// An unsigned JWT whose payload claims `tenant: globex` must NOT select a tenant:
    /// the claim is unverified, so it is ignored and the request is rejected.
    #[tokio::test]
    async fn unsigned_jwt_tenant_claim_is_not_trusted() {
        use base64::Engine;
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"tenant":"globex"}"#);
        let token = format!("eyJhbGciOiJub25lIn0.{payload}.bogus");
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Without the advisory middleware (no `UnverifiedTenantHints` opt-in), the extractor
    /// must not trust a client-supplied `X-Tenant-Id` header.
    #[tokio::test]
    async fn extractor_alone_does_not_trust_header() {
        let router = Router::new().route(
            "/whoami",
            get(|TenantContext { id, slug }: TenantContext| async move { format!("{slug}:{id}") }),
        );
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .header(TENANT_HEADER, "acme")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Reserved/apex labels such as `www` must not become tenant slugs.
    #[tokio::test]
    async fn reserved_subdomain_is_not_a_tenant() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .header(HOST, "www.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// A tenant established by an upstream verifying layer (here simulated by inserting
    /// a `TenantContext` before the middleware runs) must not be overridden by a spoofed
    /// `X-Tenant-Id` header.
    #[tokio::test]
    async fn preverified_tenant_is_not_overridden_by_header() {
        async fn verified_upstream(mut req: Request<Body>, next: Next) -> Response {
            let slug = TenantSlug("acme".to_string());
            req.extensions_mut().insert(TenantContext {
                id: slug.to_id(),
                slug,
            });
            next.run(req).await
        }

        let router =
            Router::new()
                .route(
                    "/whoami",
                    get(|TenantContext { id, slug }: TenantContext| async move {
                        format!("{slug}:{id}")
                    }),
                )
                .layer(axum::middleware::from_fn(tenant_context_middleware))
                .layer(axum::middleware::from_fn(verified_upstream));

        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/whoami")
                    .header(TENANT_HEADER, "mallory")
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
        assert!(text.starts_with("acme:"), "got {text}");
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
