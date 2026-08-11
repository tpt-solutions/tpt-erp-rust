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

    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    tracing::info!(%addr, "tpt-erp ledger server listening");
    serve(
        listener,
        app_default().merge(tpt_erp_observability::metrics_router()),
    )
    .await
    .expect("server error");
}
