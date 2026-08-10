//! Wasm promo-plugin glue: a [`HostContext`] that exposes live stock to the
//! `plugin` world, plus a [`PromoEngine`] that runs the compiled `promo` guest.
//!
//! The guest (see `examples/plugins/promo`) is computation-only: it reads stock
//! levels via the host `erp` interface and returns a per-SKU, stock-aware discount.
//! It can be hot-swapped at runtime via [`PromoEngine::swap_module`] without
//! restarting the host.

use std::sync::Arc;

use tpt_erp_wasm::host::HostContext;
use tpt_erp_wasm::{Money, PluginHandle, PluginRuntime, RuntimeConfig, RuntimeError};

use crate::reservation::{ReservationEngine, Sku};
use tpt_erp_primitives::Id;

/// Host read-model presented to the promo plugin. Stock is read live from the
/// reservation engine so discounts always reflect current availability.
pub struct PromoHost {
    engine: Arc<ReservationEngine>,
    tenant: String,
}

impl PromoHost {
    /// Build a host context over a reservation engine and tenant slug.
    pub fn new(engine: Arc<ReservationEngine>, tenant: impl Into<String>) -> Self {
        Self {
            engine,
            tenant: tenant.into(),
        }
    }
}

impl HostContext for PromoHost {
    fn account_balance(&self, _account: &str) -> Option<Money> {
        None
    }

    fn stock_level(&self, sku: &str) -> Option<u64> {
        // Map the sku string back to a strong id for a live read.
        match Id::<Sku>::parse(sku) {
            Ok(id) => Some(self.engine.available(id) as u64),
            Err(_) => None,
        }
    }

    fn current_tenant(&self) -> String {
        self.tenant.clone()
    }

    fn clone_box(&self) -> Box<dyn HostContext> {
        Box::new(Self {
            engine: self.engine.clone(),
            tenant: self.tenant.clone(),
        })
    }
}

/// Runs the `promo` Wasm component (or degrades gracefully when none is loaded).
pub struct PromoEngine {
    runtime: Option<PluginRuntime>,
    handle: Option<PluginHandle>,
    host: Arc<PromoHost>,
}

impl PromoEngine {
    /// An engine with no plugin attached: [`PromoEngine::discount`] returns `None`.
    pub fn without_plugin(engine: Arc<ReservationEngine>, tenant: &str) -> Self {
        Self {
            runtime: None,
            handle: None,
            host: Arc::new(PromoHost::new(engine, tenant)),
        }
    }

    /// Load a compiled `promo` component from bytes.
    pub fn with_plugin(
        wasm: &[u8],
        engine: Arc<ReservationEngine>,
        tenant: &str,
    ) -> Result<Self, RuntimeError> {
        let host = Arc::new(PromoHost::new(engine, tenant));
        let runtime = PluginRuntime::new(RuntimeConfig::default())?;
        let handle = runtime.load("promo", wasm, (*host).clone_box())?;
        Ok(Self {
            runtime: Some(runtime),
            handle: Some(handle),
            host,
        })
    }

    /// Hot-swap the running promo code without restarting the host.
    pub fn swap_module(&mut self, wasm: &[u8]) -> Result<(), RuntimeError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| RuntimeError::InvalidPlugin("no runtime loaded".into()))?;
        let handle = self.handle.as_mut().expect("runtime implies handle");
        runtime
            .load("promo", wasm, (*self.host).clone_box())
            .map(|h| {
                *handle = h;
            })
    }

    /// Compute a stock-aware discount for one line.
    ///
    /// `unit_cents` is the pre-discount unit price in minor units (cents). Returns the
    /// post-discount unit price in cents, or `None` when no plugin is loaded.
    pub fn discount(&mut self, sku: &str, qty: u64, unit_cents: i64) -> Option<i64> {
        let handle = self.handle.as_mut()?;
        let input = serde_json::json!({
            "sku": sku,
            "qty": qty,
            "unit_price": unit_cents,
        })
        .to_string();
        let out = handle.run(&input).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        v["final_price"].as_i64()
    }

    /// Whether a promo plugin is currently loaded.
    pub fn is_loaded(&self) -> bool {
        self.handle.is_some()
    }
}
