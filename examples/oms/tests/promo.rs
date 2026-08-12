//! End-to-end execution of the example `promo` plugin.
//!
//! Requires the prebuilt `examples/plugins/promo/promo.wasm` (produced by
//! `cargo build -p tpt-erp-cli` + `tpt plugin build`). If the component is absent the
//! test is skipped, so a clean checkout without the `wasm32-unknown-unknown` target
//! still passes CI. The guest is validated against the `plugin` world by
//! `tpt plugin build`, which is what makes this a "componentizes + validates" proof.
//!
//! Run the full pipeline on demand with:
//!
//! ```sh
//! cargo build -p tpt-erp-cli
//! tpt plugin build examples/plugins/promo
//! cargo test -p oms --test promo
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use oms::reservation::{ReservationEngine, Sku};
use tpt_erp_primitives::Id;
use tpt_erp_tenant::{TenantId, TenantSlug};
use tpt_erp_wasm::host::HostContext;
use tpt_erp_wasm::{Money, PluginRuntime, RuntimeConfig};

/// Host context that exposes live stock from a reservation engine to the promo guest.
struct Ctx {
    engine: Arc<ReservationEngine>,
}

impl HostContext for Ctx {
    fn account_balance(&self, _account: &str) -> Option<Money> {
        None
    }
    fn stock_level(&self, sku: &str) -> Option<u64> {
        Id::<Sku>::parse(sku)
            .ok()
            .map(|id| self.engine.available(id) as u64)
    }
    fn current_tenant(&self) -> String {
        "oms".into()
    }
    fn clone_box(&self) -> Box<dyn HostContext> {
        Box::new(Self {
            engine: self.engine.clone(),
        })
    }
}

fn demo_tenant() -> TenantId {
    TenantSlug("oms-demo".to_string()).to_id()
}

fn wasm_path() -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = base.parent()?.parent()?;
    let p = root.join("examples/plugins/promo/promo.wasm");
    if p.exists() { Some(p) } else { None }
}

#[tokio::test]
async fn promo_plugin_reads_stock_and_discounts() {
    let Some(path) = wasm_path() else {
        eprintln!("skipping: promo.wasm not built (run `tpt plugin build examples/plugins/promo`)");
        return;
    };
    let wasm = std::fs::read(&path).expect("read component bytes");

    let engine = Arc::new(ReservationEngine::new(demo_tenant()));
    engine.receive(Id::new(), 0).await.ok(); // no-op seed of an empty engine is fine
    let sku = Id::new();
    // High stock => clearance discount (>=100 units => 15% off).
    engine.receive(sku, 200).await.unwrap();

    let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
    let mut plugin = rt
        .load(
            "promo",
            demo_tenant(),
            &wasm,
            Box::new(Ctx {
                engine: engine.clone(),
            }),
        )
        .expect("promo component should satisfy the plugin world");

    let out = plugin
        .run(&format!(
            r#"{{"sku":"{}","qty":1,"unit_price":1000}}"#,
            sku.as_str()
        ))
        .expect("promo plugin should run");

    // 1000 cents, 15% clearance discount => 850.
    assert!(out.contains("final_price"), "unexpected output: {out}");
    assert!(
        out.contains("850"),
        "expected 15% off 1000 => 850, got: {out}"
    );
    assert!(out.contains("discount_pct"), "unexpected output: {out}");

    // Scarce stock => no discount (0%).
    let scarce = Id::new();
    engine.receive(scarce, 5).await.unwrap();
    let out2 = plugin
        .run(&format!(
            r#"{{"sku":"{}","qty":1,"unit_price":1000}}"#,
            scarce.as_str()
        ))
        .expect("promo plugin should run");
    assert!(
        out2.contains("\"final_price\":1000"),
        "expected no discount at low stock, got: {out2}"
    );
}
