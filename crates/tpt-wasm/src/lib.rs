//! # tpt-wasm (scaffold)
//!
//! The customization engine for TPT ERP. Planned for Phase 3:
//!
//! - `wasmtime`-based module loader with fuel and memory limits.
//! - A versioned WIT host-guest contract exposing read-only ERP data to plugins.
//! - A strict WASI binding layer: computation-only, no direct file I/O.
//! - Hot-load / hot-swap of plugins without restarting the host.
//! - A `tpt plugin build` CLI that scaffolds, compiles, and validates plugins.
//!
//! This crate currently defines the sandbox error type; the runtime lands in Phase 3.

use thiserror::Error;

/// Errors surfaced by the Wasm sandbox.
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("module exceeded its fuel/memory budget")]
    ResourceExhausted,
    #[error("plugin violated the host contract: {0}")]
    ContractViolation(String),
    #[error("failed to instantiate module: {0}")]
    Instantiation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        assert!(SandboxError::ResourceExhausted.to_string().contains("fuel"));
    }
}
