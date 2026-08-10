# tpt-erp-wasm

> Safe WebAssembly plugin runtime for TPT ERP — computation-only, resource
> limited, and hot-swappable.

`tpt-erp-wasm` lets operators extend the ERP with custom business logic (pricing,
routing, QC, …) **without forking the core and without trusting the plugin**. A
plugin is a WebAssembly *component* compiled against the `wit/erp.wit` contract;
the host runs it under three independent safety barriers.

## Safety model

1. **Computation-only.** The `plugin` world imports only `erp`, never `wasi:*`.
   A guest has no file, socket, clock, or random access — it can only *compute*.
   Any component importing something we did not wire (e.g. `wasi:*`) fails to
   instantiate.
2. **Resource limits.** Every `run` call gets a fresh fuel budget (default 100M
   instructions) and a hard memory ceiling (default 32 MiB). A runaway loop is cut
   off deterministically via `RuntimeError::ResourceExhausted`. An optional
   wall-clock epoch watchdog (default 500 ms) provides a second barrier.
3. **Hot-swap.** `PluginHandle::swap_module` replaces the running code at runtime,
   preserving the plugin name and host context. In-flight callers finish on the
   old code; new calls use the new component. No host restart.

## Host / guest contract

The host exposes a **read-only** view of ERP data through the `erp` interface
(`get_account_balance`, `get_stock_level`, `current_tenant`). Plugins never get a
mutable handle, raw DB access, or WASI. A missing entity is reported to the guest
as a contract `Error` (not a trap).

The embedding app implements [`HostContext`] — how ERP data is read
(in-memory projection, Postgres read-model, gRPC, …) is entirely up to you.

```rust
use tpt_erp_wasm::{PluginRuntime, RuntimeConfig, host::HostContext};

struct Ctx;
impl HostContext for Ctx {
    fn account_balance(&self, _: &str) -> Option<tpt_erp_wasm::Money> { None }
    fn stock_level(&self, _: &str) -> Option<u64> { None }
    fn current_tenant(&self) -> String { String::new() }
    fn clone_box(&self) -> Box<dyn HostContext> { Box::new(Ctx) }
}

let wasm = std::fs::read("plugin.wasm")?;
let runtime = PluginRuntime::new(RuntimeConfig::default())?;
let mut plugin = runtime.load("pricing", &wasm, Box::new(Ctx))?;
let out = plugin.run(r#"{"sku":"A-1"}"#)?;
println!("{out}");
```

## Invalid plugins are rejected, never crash the host

- A component importing an unprovided host function → `RuntimeError::InvalidPlugin`.
- A plain core wasm module (not a component) → `RuntimeError::InvalidPlugin`.
- A component with the wrong `run` signature → `RuntimeError::InvalidPlugin`.

In all cases the host stays alive and can load the next plugin.

## Building plugins

Use the `tpt` CLI (`tpt-erp-cli`) to scaffold, build, validate, and run plugins:

```sh
tpt plugin new myprice
tpt plugin build myprice
tpt plugin run myprice.wasm '{"sku":"A-1"}' --data host.json
```

## Status

Early development (0.1.0). The runtime, host bindings, and safety guarantees are
implemented and tested on the `wasmtime` component model. APIs may change between
releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
