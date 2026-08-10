//! Host-side test: hot-swap a running plugin without restarting the host.
//!
//! Requires the prebuilt `examples/plugins/pricing/pricing.wasm` (produced by
//! `cargo build -p tpt-erp-cli` + `tpt plugin build`). If the component is absent the
//! test is skipped, so a clean checkout without the wasm32 target still passes CI.

use std::path::PathBuf;
use std::sync::Arc;

use tpt_erp_wasm::host::{HostContext, TptHost};
use tpt_erp_wasm::{Money, PluginRuntime, RuntimeError, RuntimeConfig};

#[derive(Clone)]
struct Ctx {
    balance: Option<Money>,
}

impl HostContext for Ctx {
    fn account_balance(&self, _account: &str) -> Option<Money> {
        self.balance
    }
    fn stock_level(&self, _sku: &str) -> Option<u64> {
        None
    }
    fn current_tenant(&self) -> String {
        "acme".into()
    }
    fn clone_box(&self) -> Box<dyn HostContext> {
        Box::new(self.clone())
    }
}

fn wasm_path() -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/tpt-erp-wasm -> repo root -> examples/plugins/pricing/pricing.wasm
    let p = base
        .parent()?
        .parent()?
        .join("examples/plugins/pricing/pricing.wasm");
    p.exists().then_some(p)
}

/// Locate a built example plugin component under `examples/plugins/<name>/<name>.wasm`.
fn example_wasm(name: &str) -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = base
        .parent()?
        .parent()?
        .join(format!("examples/plugins/{name}/{name}.wasm"));
    p.exists().then_some(p)
}

#[test]
fn hot_swap_without_restart() {
    let Some(path) = wasm_path() else {
        eprintln!("skipping: pricing.wasm not built (run `tpt plugin build`)");
        return;
    };
    let wasm = std::fs::read(&path).expect("read component bytes");

    let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
    let mut plugin = rt
        .load(
            "pricing",
            &wasm,
            Box::new(Ctx {
                balance: Some(Money::new(1, 2000)),
            }),
        )
        .unwrap();

    // First call on the originally loaded code.
    let out1 = plugin
        .run(r#"{"account":"acc-1","amount":10000}"#)
        .unwrap();
    assert!(out1.contains("final_amount"));

    // Hot-swap the running code in place (here reloading the same bytes, i.e. a
    // version/config reload). The host is never recreated; in-flight references stay
    // valid and new calls use the swapped component.
    plugin.swap_module(&wasm).unwrap();
    let out2 = plugin
        .run(r#"{"account":"acc-1","amount":10000}"#)
        .unwrap();
    assert!(out2.contains("final_amount"));

    // A non-plugin payload must be rejected by swap_module, never crash the host.
    let bad = b"(module (func (export \"run\") (result i32) (i32.const 0)))";
    assert!(matches!(
        plugin.swap_module(bad),
        Err(RuntimeError::InvalidPlugin(_))
    ));

    let _ = TptHost::new(Arc::new(Ctx { balance: None }), wasmtime::StoreLimits::default());
}

/// End-to-end execution of the example `routing` plugin: it reads host stock (here
/// none) and returns a routing decision. Verifies the example plugin actually runs
/// under the sandbox, independent of shell quoting when invoking the CLI.
#[test]
fn routing_plugin_executes_end_to_end() {
    let Some(path) = example_wasm("routing") else {
        eprintln!("skipping: routing.wasm not built (run `tpt plugin build`)");
        return;
    };
    let wasm = std::fs::read(&path).expect("read component bytes");

    let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
    let mut plugin = rt
        .load("routing", &wasm, Box::new(Ctx { balance: None }))
        .unwrap();

    let out = plugin
        .run(r#"{"skus":["A-1","B-2","A-1"]}"#)
        .expect("routing plugin should run");
    // With no host stock the decision defaults to wave picking.
    assert!(out.contains("strategy"), "unexpected output: {out}");
    assert!(out.contains("wave"), "expected wave strategy, got: {out}");
}
