//! Structured logging and metrics for TPT ERP services.
//!
//! Two small helpers make the framework observable end-to-end:
//!
//! - [`init_tracing`] installs a `tracing` subscriber driven by the `RUST_LOG`
//!   environment variable, so the `deploy/values.yaml` `RUST_LOG` setting finally
//!   has something to act on.
//! - [`install_metrics`] registers the Prometheus recorder globally; [`metrics_router`]
//!   then exposes it as an Axum `/metrics` route (state-free, so it merges with any
//!   other router).

use axum::Router;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::sync::OnceLock;

static RECORDER: OnceLock<metrics_exporter_prometheus::PrometheusHandle> = OnceLock::new();

/// Install a `tracing` subscriber that reads its filter from the `RUST_LOG`
/// environment variable (e.g. `RUST_LOG=info,tpt_erp_server=debug`). Safe to call
/// once at process startup; subsequent calls are ignored.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .try_init();
}

/// Install the Prometheus metrics recorder globally. Call this once at startup; metrics
/// recorded anywhere via the `metrics` facade are then scraped from the route produced by
/// [`metrics_router`].
pub fn install_metrics() {
    if RECORDER.get().is_some() {
        return;
    }
    let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder");
    let _ = RECORDER.set(handle);
}

/// Build a state-free Axum router serving `/metrics` (Prometheus text exposition
/// format). Merge it into any existing router via `Router::merge`.
pub fn metrics_router() -> Router {
    Router::new().route("/metrics", get(metrics_handler))
}

async fn metrics_handler() -> Response {
    let body = RECORDER.get().map(|h| h.render()).unwrap_or_default();
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_recorder_installs_and_renders() {
        // Demonstrates the metrics facade is actually wired to a backend.
        install_metrics();
        metrics::counter!("tpt_test_counter").increment(1);
        let rendered = RECORDER.get().expect("recorder installed").render();
        assert!(rendered.contains("tpt_test_counter"));
    }
}
