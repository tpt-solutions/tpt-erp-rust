//! # tms — reference Fleet / TMS implementation on TPT ERP.
//!
//! A production-shaped transportation-management engine built entirely on the
//! framework's primitives:
//!
//! - [`geo`] — **geofencing**: circle/polygon containment plus `haversine` distance, and
//!   zone entry/exit events for the event bus.
//! - [`ingest`] — a **transport-agnostic GPS ingestion** pipeline (decode, back-pressured
//!   batching onto `tpt-erp-bus`, optional `mqtt` feature) with a load test.
//! - [`route`] — **route optimization**: nearest-neighbor seed + rayon-parallel 2-opt
//!   refinement, benchmarked against a naive tour.
//! - [`hos`] — a [`StateMachine`](tpt_erp_primitives::StateMachine)-derived **driver
//!   Hours-of-Service** lifecycle with an 11/14-hour rule-check layer.
//! - [`dispatch`] — a real backend home for the `examples/plugins/dispatch` Wasm guest:
//!   per-stop demand drives the plugin's dispatch scoring, hot-swappable at runtime.

pub mod dispatch;
pub mod geo;
pub mod hos;
pub mod ingest;
pub mod route;

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tpt_erp_tenant::{TenantId, TenantSlug};

use crate::dispatch::{DispatchEngine, StopDemand};

/// Build a tenant for example/demo use.
pub fn demo_tenant() -> TenantId {
    TenantSlug("tms-demo".to_string()).to_id()
}

/// A stop-scoring request for the dispatch endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRequest {
    pub stop: String,
    /// Routing weight (e.g. distance in meters) blended with demand by the plugin.
    pub weight: i64,
}

/// A stop-scoring response.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreResponse {
    pub stop: String,
    pub score: i64,
    pub plugin_loaded: bool,
}

/// The reference TMS application bundle.
#[derive(Clone)]
pub struct TmsApp {
    #[allow(dead_code)]
    tenant: TenantId,
    pub dispatch: Arc<tokio::sync::Mutex<DispatchEngine>>,
    pub bus: Option<Arc<dyn tpt_erp_bus::EventBus>>,
}

impl TmsApp {
    /// Build a demo TMS for `tenant` with no dispatch plugin loaded.
    pub fn new(tenant: TenantId) -> Self {
        Self {
            tenant,
            dispatch: Arc::new(tokio::sync::Mutex::new(DispatchEngine::without_plugin(
                HashMap::new(),
                "tms",
            ))),
            bus: None,
        }
    }

    /// Load the `dispatch` Wasm component to drive stop scoring.
    pub fn with_dispatch(mut self, wasm: &[u8]) -> Result<Self, tpt_erp_wasm::RuntimeError> {
        let engine = DispatchEngine::with_plugin(wasm, HashMap::new(), "tms")?;
        self.dispatch = Arc::new(tokio::sync::Mutex::new(engine));
        Ok(self)
    }

    /// Attach a background-job bus (for `gps.telemetry` / zone events).
    pub fn with_bus(mut self, bus: Arc<dyn tpt_erp_bus::EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Score a stop using the dispatch plugin (if loaded).
    pub async fn score_stop(&self, stop: &str, weight: i64) -> Option<i64> {
        let mut engine = self.dispatch.lock().await;
        engine.score(stop, weight)
    }

    /// Set the demand for a stop used by the dispatch plugin.
    pub async fn set_stop_demand(&self, stop: &str, demand: StopDemand) {
        let engine = self.dispatch.lock().await;
        // Demand lives on the host; update via the engine's host handle.
        if let Some(host) = engine.host_ref() {
            host.set_demand(stop, demand).await;
        }
    }

    /// Build the Axum router: a `/dispatch/score` handler.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/dispatch/score", post(score_handler))
            .with_state(TmsState {
                app: self.clone(),
            })
    }
}

/// Axum state shared by the TMS handlers.
#[derive(Clone)]
pub struct TmsState {
    app: TmsApp,
}

async fn score_handler(
    State(st): State<TmsState>,
    Json(req): Json<ScoreRequest>,
) -> Result<Json<ScoreResponse>, (StatusCode, String)> {
    let score = st.app.score_stop(&req.stop, req.weight).await;
    let plugin_loaded = st.app.dispatch.lock().await.is_loaded();
    match score {
        Some(score) => Ok(Json(ScoreResponse {
            stop: req.stop,
            score,
            plugin_loaded,
        })),
        None => Err((StatusCode::SERVICE_UNAVAILABLE, "no dispatch plugin loaded".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_scores_without_plugin_returns_none() {
        let app = TmsApp::new(demo_tenant());
        assert_eq!(app.score_stop("S1", 100).await, None);
    }
}
