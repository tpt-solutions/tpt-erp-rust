//! Wasm dispatch-plugin glue: the host that backs `examples/plugins/dispatch`.
//!
//! The `dispatch` guest (see `examples/plugins/dispatch`) is computation-only: it *reads*
//! a stop's demand from the host via the `erp` interface and returns a dispatch score /
//! priority used to sequence the route. Here it gets a genuine host — [`TmsDispatchHost`]
//! answers `stock_level` with a stop's delivery demand — and can be hot-swapped at runtime
//! via [`DispatchEngine::swap_module`] without restarting the host.

use std::sync::Arc;

use tpt_erp_tenant::TenantSlug;
use tpt_erp_wasm::host::HostContext;
use tpt_erp_wasm::{Money, PluginHandle, PluginRuntime, RuntimeConfig, RuntimeError};

/// Per-stop demand the dispatcher uses to prioritize. Keyed in the host by stop id.
pub type StopDemand = u64;

/// Host read-model presented to the dispatch plugin. `stock_level` is reused as the
/// stop-demand channel so the plugin stays within the computation-only `erp` contract.
pub struct TmsDispatchHost {
    demand: Arc<tokio::sync::Mutex<std::collections::HashMap<String, StopDemand>>>,
    tenant: String,
}

impl TmsDispatchHost {
    /// Build a host context from a stop→demand map and tenant slug.
    pub fn new(
        demand: std::collections::HashMap<String, StopDemand>,
        tenant: impl Into<String>,
    ) -> Self {
        Self {
            demand: Arc::new(tokio::sync::Mutex::new(demand)),
            tenant: tenant.into(),
        }
    }

    /// Update the demand for a stop (e.g. as new orders arrive).
    pub async fn set_demand(&self, stop: &str, demand: StopDemand) {
        self.demand.lock().await.insert(stop.to_string(), demand);
    }
}

impl HostContext for TmsDispatchHost {
    fn account_balance(&self, _account: &str) -> Option<Money> {
        None
    }

    fn stock_level(&self, sku: &str) -> Option<u64> {
        // A blocking lock is fine: the host call is synchronous and short.
        self.demand.blocking_lock().get(sku).copied()
    }

    fn current_tenant(&self) -> String {
        self.tenant.clone()
    }

    fn clone_box(&self) -> Box<dyn HostContext> {
        Box::new(Self {
            demand: self.demand.clone(),
            tenant: self.tenant.clone(),
        })
    }
}

/// Runs the `dispatch` Wasm component (or degrades gracefully when none is loaded).
pub struct DispatchEngine {
    runtime: Option<PluginRuntime>,
    handle: Option<PluginHandle>,
    host: Arc<TmsDispatchHost>,
}

impl DispatchEngine {
    /// An engine with no plugin attached: [`DispatchEngine::score`] returns `None`.
    pub fn without_plugin(
        demand: std::collections::HashMap<String, StopDemand>,
        tenant: &str,
    ) -> Self {
        Self {
            runtime: None,
            handle: None,
            host: Arc::new(TmsDispatchHost::new(demand, tenant)),
        }
    }

    /// Load a compiled `dispatch` component from bytes.
    pub fn with_plugin(
        wasm: &[u8],
        demand: std::collections::HashMap<String, StopDemand>,
        tenant: &str,
    ) -> Result<Self, RuntimeError> {
        let host = Arc::new(TmsDispatchHost::new(demand, tenant));
        let runtime = PluginRuntime::new(RuntimeConfig::default())?;
        let handle = runtime.load("dispatch", TenantSlug(tenant.to_string()).to_id(), wasm, (*host).clone_box())?;
        Ok(Self {
            runtime: Some(runtime),
            handle: Some(handle),
            host,
        })
    }

    /// Hot-swap the running dispatch code without restarting the host.
    pub fn swap_module(&mut self, wasm: &[u8]) -> Result<(), RuntimeError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| RuntimeError::InvalidPlugin("no runtime loaded".into()))?;
        let handle = self.handle.as_mut().expect("runtime implies handle");
        runtime
            .load("dispatch", TenantSlug(self.host.current_tenant()).to_id(), wasm, (*self.host).clone_box())
            .map(|h| {
                *handle = h;
            })
    }

    /// Compute a dispatch score / priority for a stop.
    ///
    /// `weight` is a routing weight (e.g. distance in meters) the plugin blends with the
    /// stop's demand. Returns the plugin's score, or `None` when no plugin is loaded.
    pub fn score(&mut self, stop: &str, weight: i64) -> Option<i64> {
        let handle = self.handle.as_mut()?;
        let input = serde_json::json!({
            "stop": stop,
            "weight": weight,
        })
        .to_string();
        let out = handle.run(&input).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        v["score"].as_i64()
    }

    /// Whether a dispatch plugin is currently loaded.
    pub fn is_loaded(&self) -> bool {
        self.handle.is_some()
    }

    /// A clone of the host read-model backing the plugin (always present), so callers can
    /// update per-stop demand without reloading the component.
    pub fn host_ref(&self) -> Option<Arc<TmsDispatchHost>> {
        Some(self.host.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locate the prebuilt `examples/plugins/dispatch/dispatch.wasm`.
    fn wasm_path() -> Option<std::path::PathBuf> {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let p = base
            .parent()?
            .parent()?
            .join("examples/plugins/dispatch/dispatch.wasm");
        p.exists().then_some(p)
    }

    #[test]
    fn without_plugin_returns_no_score() {
        let mut eng = DispatchEngine::without_plugin(std::collections::HashMap::new(), "tms-demo");
        assert!(!eng.is_loaded());
        assert_eq!(eng.score("S1", 100), None);
    }

    #[test]
    fn dispatch_plugin_scores_a_stop() {
        let Some(path) = wasm_path() else {
            eprintln!("skipping: dispatch.wasm not built (run `tpt plugin build`)");
            return;
        };
        let wasm = std::fs::read(&path).expect("read component bytes");

        let mut demand = std::collections::HashMap::new();
        demand.insert("S1".to_string(), 50);
        let mut eng = DispatchEngine::with_plugin(&wasm, demand, "tms-demo").unwrap();
        assert!(eng.is_loaded());

        let score = eng.score("S1", 100).expect("dispatch plugin should score");
        assert!(score > 0, "expected a positive score, got {score}");

        // Hot-swap the same code in place — the host is never recreated.
        let wasm2 = std::fs::read(&path).expect("read component bytes");
        eng.swap_module(&wasm2).unwrap();
        let again = eng.score("S1", 100).unwrap();
        assert_eq!(again, score);
    }
}
