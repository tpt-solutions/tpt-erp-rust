//! Entry point for the reference multi-tenant ledger server.

use axum::serve;
use server::app_default;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let addr = std::env::var("TPT_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string())
        .parse::<SocketAddr>()
        .expect("invalid TPT_BIND address");

    let listener = TcpListener::bind(addr).await.expect("failed to bind");
    println!("tpt-erp ledger server listening on http://{addr}");
    serve(listener, app_default()).await.expect("server error");
}
