# TPT ERP RUST

> **Correctness by Compilation. Customization by Sandboxing. Scale by Default.**

`tpt-erp-rust` is a high-performance, type-safe Rust **framework** (not a monolithic
app) for building domain-specific ERP systems. It attacks the three historical killers
of ERP projects — data corruption, customization forks, and batch bottlenecks — with:

- **Rust's type system** — invalid states and cross-entity mistakes become compile errors.
- **WebAssembly plugins** — client-specific business logic runs sandboxed, hot-loaded, no forks.
- **Async-first architecture** — tokio + event sourcing handles high-throughput event streams.

## Ecosystem (core crates)

| Crate             | Purpose                                                                 |
|-------------------|-------------------------------------------------------------------------|
| `tpt-primitives`  | Domain modeling: `Money<C>`, `Id<T>` strong IDs, `StateMachine` derive. |
| `tpt-macros`      | Proc-macros: `#[derive(StateMachine)]`, `TptEntity`, `TptApi` (planned).|
| `tpt-ledger`      | Event-sourced double-entry core + CQRS projection engine (planned).     |
| `tpt-tenant`      | Multi-tenancy via Postgres RLS + per-request tenant context (planned).  |
| `tpt-wasm`        | `wasmtime` sandbox for safe, hot-loadable business-logic plugins.       |

## Project layout

```
crates/      # library crates (the framework)
examples/    # runnable reference apps / quickstarts
docs/        # architecture & usage documentation
```

## Status

This repository is in **Phase 1: The Foundation**. See [`todo.md`](./todo.md) for the
full roadmap and current progress.

- [x] Workspace + `tpt-primitives` (`Money`, `Id`) and `StateMachine` derive macro
- [ ] `tpt-ledger`, `tpt-tenant`, `tpt-wasm` skeletons
- [ ] Reference implementations (3PL/WMS, Manufacturing/MES)

## Quickstart

A developer should be able to spin up a type-safe CRUD API in under 10 minutes. The
[`examples/quickstart`](./examples/quickstart) crate demonstrates defining business
types with `tpt-primitives` and the `StateMachine` macro.

```bash
cargo run -p quickstart
```

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](./LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](./LICENSE-MIT))

at your option.

Copyright © 2026 TPT Solutions.
