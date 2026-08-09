//! Axum integration for tenant resolution (enabled by the `axum` feature).
//!
//! - [`TenantContext`] is an Axum extractor: any handler parameter `TenantContext` will
//!   be populated from the request, resolving the tenant from a `Host` subdomain or the
//!   `X-Tenant-Id` header (in that priority order).
//! - [`tenant_context_middleware`] is an optional middleware that pre-resolves the tenant
//!   and stashes it in request extensions; this is also where a live database connection
//!   would issue `SET LOCAL app.tenant_id = '<uuid>'` per transaction.

use crate::identification::{TenantSlug, from_header, from_subdomain};
use crate::rls::set_tenant_command;
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
    if let Some(host) = headers.get(HOST).and_then(|v| v.to_str().ok()) {
        if let Some(slug) = from_subdomain(host) {
            return Some(slug);
        }
    }
    headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(from_header)
}

/// Middleware that resolves the tenant once and records it in request extensions, and
/// computes the `SET LOCAL` command that a DB pool would execute for the transaction.
pub async fn tenant_context_middleware(req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    if let Some(slug) = resolve_slug(&parts.headers) {
        if slug.validate().is_ok() {
            let ctx = TenantContext {
                id: slug.to_id(),
                slug: slug.clone(),
            };
            // The SQL a connection would run for this transaction (idempotent per request).
            let _set_local = set_tenant_command(&ctx.id);
            parts.extensions.insert(ctx);
        }
    }
    let req = Request::from_parts(parts, body);
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
