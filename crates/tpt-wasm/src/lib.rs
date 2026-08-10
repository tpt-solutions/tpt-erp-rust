//! Sandboxed WebAssembly plugin runtime for TPT ERP.
//!
//! This crate lets operators extend the ERP with custom business logic
//! *safely*: a plugin is a WebAssembly component compiled against the
//! [`wit/erp.wit`](https://github.com/tpt-erp-rust/tpt-erp-rust) contract.
//! The host runs it under three independent safety barriers:
//!
//! 1. **Computation-only** — the `plugin` world imports only `erp`, never
//!    `wasi:*`, so a guest has no file, socket, clock, or random access.
//! 2. **Resource limits** — every instance runs with a fuel budget
//!    (instructions) and a hard memory ceiling (see [`RuntimeConfig`]).
//! 3. **Hot-swap** — plugins can be replaced at runtime via
//!    [`PluginHandle::swap_module`] without restarting the host.
//!
//! ```no_run
//! use tpt_wasm::{PluginRuntime, RuntimeConfig, host::HostContext};
//!
//! # struct Ctx;
//! # impl HostContext for Ctx {
//! #     fn account_balance(&self, _: &str) -> Option<tpt_wasm::Money> { None }
//! #     fn stock_level(&self, _: &str) -> Option<u64> { None }
//! #     fn current_tenant(&self) -> String { String::new() }
//! #     fn clone_box(&self) -> Box<dyn HostContext> { Box::new(Ctx) }
//! # }
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let wasm: Vec<u8> = std::fs::read("plugin.wasm")?;
//! let runtime = PluginRuntime::new(RuntimeConfig::default())?;
//! let mut plugin = runtime.load("pricing", &wasm, Box::new(Ctx))?;
//! let out = plugin.run(r#"{"sku":"A-1"}"#)?;
//! println!("{out}");
//! # Ok(()) }
//! ```

mod contract;
pub mod host;
mod runtime;

pub use contract::Plugin;
pub use runtime::{PluginHandle, PluginRuntime, RuntimeConfig, RuntimeError};

use serde::{Deserialize, Serialize};

/// A decimal amount portable across the host/guest boundary.
///
/// Mirrors the WIT `money` record: major units plus a 1/10_000ths minor
/// component, sidestepping floating-point drift and the lack of a
/// 128-bit decimal in some guest languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// Whole currency units.
    pub major: i64,
    /// Fractional units in 1/10_000ths (4 decimal places).
    pub minor: i64,
}

impl Money {
    /// Build from major + 1/10_000ths minor components.
    pub fn new(major: i64, minor: i64) -> Self {
        Self { major, minor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::tpt::erp::erp::Host;
    use crate::host::{HostContext, TptHost};
    use crate::runtime::{PluginRuntime, RuntimeConfig, RuntimeError};

    /// A trivial in-memory read-model for tests.
    #[derive(Clone)]
    struct MockCtx {
        balance: Option<Money>,
        stock: Option<u64>,
        tenant: String,
    }

    impl HostContext for MockCtx {
        fn account_balance(&self, account: &str) -> Option<Money> {
            if account == "acc-1" {
                self.balance
            } else {
                None
            }
        }
        fn stock_level(&self, sku: &str) -> Option<u64> {
            if sku == "sku-1" { self.stock } else { None }
        }
        fn current_tenant(&self) -> String {
            self.tenant.clone()
        }
        fn clone_box(&self) -> Box<dyn HostContext> {
            Box::new(self.clone())
        }
    }

    // The host `erp` interface implementation must surface a missing
    // entity as a contract `Error` rather than a trap, and must forward
    // real data unchanged.
    #[test]
    fn host_binding_translates_reads_to_contract() {
        let mut host = TptHost::new(
            std::sync::Arc::new(MockCtx {
                balance: Some(Money::new(12, 3400)),
                stock: Some(7),
                tenant: "acme".into(),
            }),
            wasmtime::StoreLimits::default(),
        );

        let bal = host.get_account_balance("acc-1".into()).unwrap();
        assert_eq!((bal.major, bal.minor), (12, 3400));

        let stock = host.get_stock_level("sku-1".into()).unwrap();
        assert_eq!(stock, 7);

        assert_eq!(host.current_tenant(), "acme");

        // Unknown entity -> contract error, not a panic/trap.
        let missing = host.get_account_balance("ghost".into());
        assert!(missing.is_err());
    }

    #[test]
    fn runtime_builds_with_sandbox_defaults() {
        let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
        let _ = rt; // engine + linker configured
    }

    // A component that imports a host function we never provide must fail
    // to instantiate. This is the core "computation-only / no-WASI"
    // guarantee: we only wire `erp`, so any other import (including
    // `wasi:*`) is unresolved and rejected before it can run.
    #[test]
    fn rejects_component_with_unknown_host_import() {
        // Component importing an unprovided `host:evil` function.
        let wat = r#"
            (component
                (import "host:evil" (func "boom" (param string) (result string)))
            )
        "#;
        let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
        let ctx = MockCtx {
            balance: None,
            stock: None,
            tenant: String::new(),
        };
        let res = rt.load("evil", wat.as_bytes(), Box::new(ctx));
        assert!(matches!(res, Err(RuntimeError::InvalidPlugin(_))));
    }

    // Feeding a plain core wasm module (not a component) must be rejected
    // rather than misinterpreted.
    #[test]
    fn rejects_core_module_not_component() {
        let wat = r#"
            (module
                (func (export "run") (result i32) (i32.const 0))
                (memory (export "memory") 1)
            )
        "#;
        let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
        let ctx = MockCtx {
            balance: None,
            stock: None,
            tenant: String::new(),
        };
        let res = rt.load("core", wat.as_bytes(), Box::new(ctx));
        assert!(matches!(res, Err(RuntimeError::InvalidPlugin(_))));
    }

    // A malformed component whose `run` export has the wrong signature
    // (here `func() -> s64` instead of the contract's
    // `func(string) -> result<string,string>`) must be rejected as an
    // invalid plugin rather than panicking or crashing the host. This is
    // the "broken module can't crash the host" safety guarantee.
    //
    // (A fully valid looping plugin is exercised end-to-end by the
    // `tpt plugin build` example in Phase 3, which compiles a real
    // guest; the fuel budget is wired via `RuntimeConfig::fuel_per_call`
    // and `Config::consume_fuel` and enforced by wasmtime per call.)
    #[test]
    fn broken_plugin_signature_is_rejected_safely() {
        // `run` has the wrong type, so it can't satisfy the `plugin`
        // world even though it is syntactically a valid component.
        let wat = r#"
            (component
                (core module $m
                    (memory (export "memory") 1)
                    (func (export "run") (result i32) (i32.const 0))
                    (func (export "canonical_abi_realloc")
                        (param i32 i32 i32 i32) (result i32) (i32.const 0))
                )
                (core instance $i (instantiate $m))
                (func (export "run") (canon lift (core func $i "run")))
            )
        "#;
        let rt = PluginRuntime::new(RuntimeConfig::default()).unwrap();
        let ctx = MockCtx {
            balance: None,
            stock: None,
            tenant: String::new(),
        };
        let res = rt.load("broken", wat.as_bytes(), Box::new(ctx));
        assert!(matches!(res, Err(RuntimeError::InvalidPlugin(_))));
    }
}
