//! Entry point for the reference multi-tenant ledger server.

use axum::serve;
use server::app_default;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    // Structured logging (RUST_LOG) + Prometheus metrics endpoint.
    tpt_erp_observability::init_tracing();
    tpt_erp_observability::install_metrics();

    let addr = std::env::var("TPT_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string())
        .parse::<SocketAddr>()
        .expect("invalid TPT_BIND address");

    // Fail closed: without a JWT secret the tenant comes from unauthenticated Host/header
    // hints, which anyone can forge. Require an explicit opt-in for that dev-only mode.
    let has_secret = std::env::var("TPT_JWT_SECRET").is_ok_and(|s| !s.is_empty());
    let allow_unverified = std::env::var("TPT_ALLOW_UNVERIFIED_TENANT")
        .is_ok_and(|v| matches!(v.as_str(), "1" | "true"));
    if !has_secret && !allow_unverified {
        eprintln!(
            "refusing to start: set TPT_JWT_SECRET (production) or \
             TPT_ALLOW_UNVERIFIED_TENANT=1 (local dev only, trusts X-Tenant-Id/Host)"
        );
        std::process::exit(1);
    }

    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    tracing::info!(%addr, "tpt-erp ledger server listening");
    serve(
        listener,
        app_default().merge(tpt_erp_observability::metrics_router()),
    )
    .await
    .expect("server error");
}
