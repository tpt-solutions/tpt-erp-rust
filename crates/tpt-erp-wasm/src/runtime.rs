//! The plugin runtime: engine configuration, module loading, execution,
//! and hot-swap.
//!
//! Two types live here:
//!
//! - [`PluginRuntime`] — owns the shared wasmtime [`Engine`] and the
//!   [`Linker`] pre-wired with the `erp` host interface. Cheap to create
//!   and share across threads.
//! - [`PluginHandle`] — a single instantiated plugin with its own
//!   [`Store`] (and therefore its own fuel/memory budget and host
//!   context). Supports [`PluginHandle::swap_module`] for hot-swapping
//!   the underlying code without dropping in-flight callers' references.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasmtime::{
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
    component::{Component, Linker},
};

use tpt_erp_tenant::TenantId;

use crate::billing::UsageMeter;
use crate::contract::Plugin as PluginBindings;
use crate::host::{HostContext, TptHost};

/// Configuration for the sandbox: resource ceilings applied per plugin.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Maximum WebAssembly instructions (fuel) per `run` call.
    ///
    /// Fuel is a deterministic proxy for CPU time: it counts executed
    /// operations, so a runaway or malicious loop is cut off regardless
    /// of the host's clock. Default 100 million.
    pub fuel_per_call: u64,
    /// Hard memory ceiling in bytes for a single plugin instance.
    ///
    /// Default 32 MiB. The guest cannot allocate beyond this; an attempt
    /// traps with `ResourceExhausted`.
    pub max_memory_bytes: usize,
    /// Optional wall-clock cap; the host interrupts the guest when
    /// exceeded via an epoch deadline. `None` disables the watchdog
    /// (fuel remains the primary barrier).
    pub wall_clock: Option<Duration>,
    /// Optional hard cap on the compiled component size in bytes, checked *before*
    /// `wasmtime` compiles it. This bounds memory/CPU spent on pathological or
    /// accidentally-huge modules, ahead of the per-call fuel/memory limits taking
    /// effect during execution. `None` disables the pre-check.
    pub max_module_bytes: Option<usize>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            fuel_per_call: 100_000_000,
            max_memory_bytes: 32 * 1024 * 1024,
            wall_clock: Some(Duration::from_millis(500)),
            max_module_bytes: Some(8 * 1024 * 1024),
        }
    }
}

/// Errors raised while loading, instantiating, or running a plugin.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// wasmtime failed to compile the bytes into a component.
    #[error("failed to compile component: {0}")]
    Compile(#[from] wasmtime::Error),
    /// The component does not expose the required `plugin` world exports.
    #[error("component is not a valid tpt:erp plugin: {0}")]
    InvalidPlugin(String),
    /// The guest exceeded its fuel or memory budget.
    #[error("plugin exceeded its resource budget")]
    ResourceExhausted,
    /// The guest's `run` returned an application error string.
    #[error("plugin run failed: {0}")]
    PluginError(String),
}

/// Shared runtime: engine + linker. Create once, load many plugins.
pub struct PluginRuntime {
    engine: Engine,
    linker: Linker<TptHost>,
    config: RuntimeConfig,
    /// Shared per-tenant usage meter; consulted on every plugin call for billing.
    usage: Arc<UsageMeter>,
    /// Background ticker that advances wasmtime's epoch so the wall-clock
    /// cap (set as a 1-epoch deadline per `run`) actually takes effect.
    _epoch_ticker: Option<EpochTicker>,
}

/// A background thread that periodically calls `Engine::increment_epoch`,
/// paired with a stop flag so it can be joined on drop.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl PluginRuntime {
    /// Build a runtime with the given sandbox configuration.
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true);
        // Deterministic, portable execution (no SIMD host tricks needed
        // for our plugins) and the fuel meter used for the CPU barrier.
        cfg.consume_fuel(true);
        cfg.memory_init_cow(true);
        if config.wall_clock.is_some() {
            cfg.epoch_interruption(true);
        }
        let engine = Engine::new(&cfg)?;

        let epoch_ticker = config.wall_clock.map(|interval| {
            let ticker = engine.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let handle = thread::Builder::new()
                .name("tpt-wasm-epoch".to_string())
                .spawn({
                    let stop = Arc::clone(&stop);
                    move || {
                        while !stop.load(Ordering::SeqCst) {
                            thread::sleep(interval);
                            if stop.load(Ordering::SeqCst) {
                                break;
                            }
                            ticker.increment_epoch();
                        }
                    }
                })
                .expect("failed to spawn wasm epoch ticker");
            EpochTicker {
                stop,
                handle: Some(handle),
            }
        });

        let mut linker: Linker<TptHost> = Linker::new(&engine);
        // Wire the `erp` host interface into every guest. No WASI is
        // linked, which is what keeps plugins computation-only.
        PluginBindings::add_to_linker(&mut linker, |host: &mut TptHost| host)?;

        Ok(Self {
            engine,
            linker,
            config,
            usage: Arc::new(UsageMeter::new()),
            _epoch_ticker: epoch_ticker,
        })
    }

    /// Access the shared per-tenant usage meter (for billing rollups).
    pub fn usage_meter(&self) -> Arc<UsageMeter> {
        Arc::clone(&self.usage)
    }

    /// Load and instantiate a plugin from compiled component bytes.
    ///
    /// `name` is an operator-facing label used in errors/logs. `tenant` is the tenant the
    /// plugin runs on behalf of; fuel consumed by its `run` calls is metered against that
    /// tenant via the shared [`UsageMeter`]. `ctx` is the host read-model the plugin will
    /// query through `erp`.
    pub fn load(
        &self,
        name: &str,
        tenant: TenantId,
        wasm: &[u8],
        ctx: Box<dyn HostContext>,
    ) -> Result<PluginHandle, RuntimeError> {
        if let Some(cap) = self.config.max_module_bytes
            && wasm.len() > cap
        {
            return Err(RuntimeError::InvalidPlugin(format!(
                "module is {} bytes, exceeding the {} byte limit",
                wasm.len(),
                cap
            )));
        }
        let component = Component::new(&self.engine, wasm)
            .map_err(|e| RuntimeError::InvalidPlugin(e.to_string()))?;

        let ctx = Arc::from(ctx);
        let handle = self.build_handle(name, tenant, component, ctx);
        Ok(handle)
    }

    fn build_handle(
        &self,
        name: &str,
        tenant: TenantId,
        component: Component,
        ctx: Arc<dyn HostContext>,
    ) -> PluginHandle {
        let mut handle = PluginHandle {
            name: name.to_string(),
            tenant,
            runtime: PluginRuntimeRef {
                engine: self.engine.clone(),
                linker: self.linker.clone(),
                config: self.config.clone(),
                usage: Arc::clone(&self.usage),
            },
            component: Arc::new(component),
            ctx: Arc::clone(&ctx),
            store: Store::new(
                &self.engine,
                TptHost::new(Arc::clone(&ctx), StoreLimits::default()),
            ),
        };
        handle.reset_store();
        handle
    }
}

// Cheap-to-clone handles to the shared engine/linker/config so a
// `PluginHandle` can rebuild its store on hot-swap.
#[derive(Clone)]
struct PluginRuntimeRef {
    engine: Engine,
    linker: Linker<TptHost>,
    config: RuntimeConfig,
    usage: Arc<UsageMeter>,
}

/// A live, instantiated plugin.
///
/// Calls are serialized on the handle's [`Store`]; use one handle per
/// concurrent caller if true parallelism is required. [`PluginHandle`]
/// is not `Clone` because the store (and its per-call fuel budget) is
/// owned and mutable.
pub struct PluginHandle {
    name: String,
    /// The tenant this plugin runs on behalf of (for usage billing).
    tenant: TenantId,
    runtime: PluginRuntimeRef,
    component: Arc<Component>,
    ctx: Arc<dyn HostContext>,
    store: Store<TptHost>,
}

impl PluginHandle {
    /// Operator-facing plugin name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tenant this plugin is billed against.
    pub fn tenant(&self) -> TenantId {
        self.tenant
    }

    /// Reset the store with a fresh fuel budget and memory limiter.
    /// Called before each `run` and after a swap.
    fn reset_store(&mut self) {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.runtime.config.max_memory_bytes)
            .build();
        let mut store = Store::new(
            &self.runtime.engine,
            TptHost::new(Arc::from(self.ctx.clone_box()), limits),
        );
        store
            .set_fuel(self.runtime.config.fuel_per_call)
            .expect("fuel enabled");
        if self.runtime.config.wall_clock.is_some() {
            store.set_epoch_deadline(1);
        }
        store.limiter(|state: &mut TptHost| &mut state.limits);
        self.store = store;
    }

    /// Replace the running code with a freshly compiled component,
    /// preserving the plugin name and host context. In-flight callers
    /// finish on the old code; new calls use the new component. No host
    /// restart required.
    pub fn swap_module(&mut self, wasm: &[u8]) -> Result<(), RuntimeError> {
        if let Some(cap) = self.runtime.config.max_module_bytes
            && wasm.len() > cap
        {
            return Err(RuntimeError::InvalidPlugin(format!(
                "module is {} bytes, exceeding the {} byte limit",
                wasm.len(),
                cap
            )));
        }
        let component = Component::new(&self.runtime.engine, wasm)
            .map_err(|e| RuntimeError::InvalidPlugin(e.to_string()))?;

        self.component = Arc::new(component);
        self.reset_store();
        Ok(())
    }

    /// Invoke the plugin's `run` with a JSON `input` string and return
    /// its JSON output string.
    ///
    /// Fuel is reset before each call, so the budget is per-call, not
    /// per-handle-lifetime. A budget overrun surfaces as
    /// [`RuntimeError::ResourceExhausted`].
    pub fn run(&mut self, input: &str) -> Result<String, RuntimeError> {
        self.reset_store();
        let fuel_before = self.runtime.config.fuel_per_call;

        let bindings =
            PluginBindings::instantiate(&mut self.store, &self.component, &self.runtime.linker)
                .map_err(|e| RuntimeError::InvalidPlugin(e.to_string()))?;

        let run = bindings
            .call_run(&mut self.store, input)
            .map_err(map_trap)?;

        // Meter the wasm fuel actually consumed this call and attribute it to the tenant.
        let fuel_remaining = self.store.get_fuel().unwrap_or(0);
        let consumed = fuel_before.saturating_sub(fuel_remaining);
        self.runtime.usage.record_call(self.tenant, consumed);

        run.map_err(RuntimeError::PluginError)
    }
}

/// Translate a wasmtime trap (including fuel/memory exhaustion) into our
/// error type. Fuel/memory/epoch exhaustion is reported as
/// `ResourceExhausted`; anything else from the host glue is surfaced
/// verbatim.
fn map_trap(e: wasmtime::Error) -> RuntimeError {
    let msg = e.to_string();
    if msg.contains("fuel")
        || msg.contains("out of memory")
        || msg.contains("memory")
        || msg.contains("epoch")
        || msg.contains("interrupt")
    {
        RuntimeError::ResourceExhausted
    } else {
        RuntimeError::InvalidPlugin(msg)
    }
}
