//! Wasm pricing-plugin glue: the real backend home for `examples/plugins/pricing`.
//!
//! The `pricing` guest (see `examples/plugins/pricing`) is computation-only: it *reads*
//! a balance from the host via the `erp` interface and returns a balance-tiered
//! discount. Here it gets a genuine host — [`PosPricingHost`] answers `account_balance`
//! with the store's loyalty balance, so the same plugin that shipped as a Phase 3 demo
//! now drives live, per-transaction discounting at the register. It can be hot-swapped
//! at runtime via [`PosPricingEngine::swap_module`] without restarting the host.

use std::sync::Arc;

use tpt_erp_wasm::host::HostContext;
use tpt_erp_wasm::{Money, PluginHandle, PluginRuntime, RuntimeConfig, RuntimeError};

/// Host read-model presented to the pricing plugin. The store's loyalty balance is the
/// single value the plugin reads to choose a discount tier.
pub struct PosPricingHost {
    store_balance: Arc<tokio::sync::Mutex<Money>>,
    tenant: String,
}

impl PosPricingHost {
    /// Build a host context over the store's loyalty balance and tenant slug.
    pub fn new(store_balance: Money, tenant: impl Into<String>) -> Self {
        Self {
            store_balance: Arc::new(tokio::sync::Mutex::new(store_balance)),
            tenant: tenant.into(),
        }
    }

    /// Update the store balance the plugin will read (e.g. after a loyalty accrual).
    pub async fn set_balance(&self, balance: Money) {
        *self.store_balance.lock().await = balance;
    }
}

impl HostContext for PosPricingHost {
    fn account_balance(&self, _account: &str) -> Option<Money> {
        // A blocking lock is fine here: the host call is synchronous and short.
        Some(*self.store_balance.blocking_lock())
    }

    fn stock_level(&self, _sku: &str) -> Option<u64> {
        None
    }

    fn current_tenant(&self) -> String {
        self.tenant.clone()
    }

    fn clone_box(&self) -> Box<dyn HostContext> {
        Box::new(Self {
            store_balance: self.store_balance.clone(),
            tenant: self.tenant.clone(),
        })
    }
}

/// Runs the `pricing` Wasm component (or degrades gracefully when none is loaded).
pub struct PosPricingEngine {
    runtime: Option<PluginRuntime>,
    handle: Option<PluginHandle>,
    host: Arc<PosPricingHost>,
}

impl PosPricingEngine {
    /// An engine with no plugin attached: [`PosPricingEngine::discount`] returns `None`.
    pub fn without_plugin(store_balance: Money, tenant: &str) -> Self {
        Self {
            runtime: None,
            handle: None,
            host: Arc::new(PosPricingHost::new(store_balance, tenant)),
        }
    }

    /// Load a compiled `pricing` component from bytes.
    pub fn with_plugin(
        wasm: &[u8],
        store_balance: Money,
        tenant: &str,
    ) -> Result<Self, RuntimeError> {
        let host = Arc::new(PosPricingHost::new(store_balance, tenant));
        let runtime = PluginRuntime::new(RuntimeConfig::default())?;
        let handle = runtime.load("pricing", wasm, (*host).clone_box())?;
        Ok(Self {
            runtime: Some(runtime),
            handle: Some(handle),
            host,
        })
    }

    /// Hot-swap the running pricing code without restarting the host.
    pub fn swap_module(&mut self, wasm: &[u8]) -> Result<(), RuntimeError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| RuntimeError::InvalidPlugin("no runtime loaded".into()))?;
        let handle = self.handle.as_mut().expect("runtime implies handle");
        runtime
            .load("pricing", wasm, (*self.host).clone_box())
            .map(|h| {
                *handle = h;
            })
    }

    /// Compute a balance-tiered discount for a transaction amount.
    ///
    /// `amount_cents` is the pre-discount transaction amount in minor units (cents).
    /// Returns the post-discount amount in cents, or `None` when no plugin is loaded.
    pub fn discount(&mut self, store_account: &str, amount_cents: i64) -> Option<i64> {
        let handle = self.handle.as_mut()?;
        let input = serde_json::json!({
            "account": store_account,
            "amount": amount_cents,
        })
        .to_string();
        let out = handle.run(&input).ok()?;
        let v: serde_json::Value = serde_json::from_str(&out).ok()?;
        v["final_amount"].as_i64()
    }

    /// Whether a pricing plugin is currently loaded.
    pub fn is_loaded(&self) -> bool {
        self.handle.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locate the prebuilt `examples/plugins/pricing/pricing.wasm`.
    fn wasm_path() -> Option<std::path::PathBuf> {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // examples/pos -> repo root -> examples/plugins/pricing/pricing.wasm
        let p = base
            .parent()?
            .parent()?
            .join("examples/plugins/pricing/pricing.wasm");
        p.exists().then_some(p)
    }

    #[test]
    fn without_plugin_returns_no_discount() {
        let mut eng = PosPricingEngine::without_plugin(Money::new(0, 0), "pos-demo");
        assert!(!eng.is_loaded());
        assert_eq!(eng.discount("store-1", 10_000), None);
    }

    #[test]
    fn pricing_plugin_applies_balance_tiered_discount() {
        let Some(path) = wasm_path() else {
            eprintln!("skipping: pricing.wasm not built (run `tpt plugin build`)");
            return;
        };
        let wasm = std::fs::read(&path).expect("read component bytes");

        // A high store balance (> 1_000_000 ⇒ 10% tier in the pricing guest).
        let mut eng = PosPricingEngine::with_plugin(&wasm, Money::new(200, 0), "pos-demo").unwrap();
        assert!(eng.is_loaded());
        let discounted = eng.discount("store-1", 10_000).unwrap();
        assert!(discounted < 10_000, "expected a discount, got {discounted}");
        assert!(discounted > 8_000, "10% off 10000 should be 9000, got {discounted}");

        // Hot-swap the same code in place — the host is never recreated.
        let wasm2 = std::fs::read(&path).expect("read component bytes");
        eng.swap_module(&wasm2).unwrap();
        let again = eng.discount("store-1", 10_000).unwrap();
        assert_eq!(again, discounted);
    }
}
