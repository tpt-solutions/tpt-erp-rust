//! Generated host/guest bindings for the TPT ERP WIT contract.
//!
//! This module is produced by `wasmtime::component::bindgen!` reading
//! [`../wit/erp.wit`]. It yields:
//!
//! - the [`tpt::erp::erp::Host`] host trait, which
//!   [`crate::host::TptHost`] implements to expose ERP reads to plugins,
//! - the [`Plugin`] world bindings used to instantiate guests,
//! - typed wrappers for the contract's `money`, `error`, and `run` types.
//!
//! Because the `plugin` world imports only `erp` (never `wasi:*`), a
//! correctly built guest is computation-only: it cannot open files or
//! sockets. The host additionally enforces fuel and memory limits at
//! runtime (see [`crate::runtime`]).

#![allow(missing_docs)]

wasmtime::component::bindgen!({
    path: "wit",
    world: "plugin",
    async: false,
});
