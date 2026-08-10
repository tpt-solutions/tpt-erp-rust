# TPT ERP RUST

> **Correctness by Compilation. Customization by Sandboxing. Scale by Default.**

`tpt-erp-rust` is a high-performance, type-safe Rust **framework** (not a monolithic
app) for building domain-specific ERP systems. It attacks the three historical killers
of ERP projects — data corruption, customization forks, and batch bottlenecks — with:

- **Rust's type system** — invalid states and cross-entity mistakes become compile errors.
- **WebAssembly plugins** — client-specific business logic runs sandboxed, hot-loaded, no forks.
- **Async-first architecture** — tokio + event sourcing handles high-throughput event streams.

## Ecosystem

| Crate | Purpose |
|-------|---------|
| `tpt-erp-primitives` | Domain modeling: `Money<C>`, `Id<T>`/`IntId<T>` strong IDs, currency markers, `StateMachine` derive. |
| `tpt-erp-macros` | Proc-macros: `#[derive(StateMachine)]`, `#[derive(TptEntity)]`, `#[derive(TptApi)]`. |
| `tpt-erp-entity` | Runtime traits for the derives + an in-memory `Repository`, validation, audit, and RBAC hooks. |
| `tpt-erp-ledger` | Event-sourced double-entry core, append-only `EventStore`, and a CQRS projection engine. |
| `tpt-erp-tenant` | Multi-tenancy via Postgres RLS + per-request tenant context (Axum extractor/middleware). |
| `tpt-erp-wasm` | `wasmtime` sandbox for safe, computation-only, hot-loadable business-logic plugins. |
| `tpt-erp-bus` | Event/pub-sub + background-job transport (NATS JetStream, in-memory reference). |
| `tpt-erp-cache` | Tenant-scoped sessions + CQRS read-model cache (Redis/Dragonfly, in-memory reference). |
| `tpt-erp-cli` | `tpt` CLI: scaffold, build, validate, and run WIT-based WebAssembly plugins. |
| `tpt-erp-frontend` | Leptos WASM storefront demo reusing `tpt-erp-primitives` types in the browser. |

## Project layout

```
crates/      # library crates (the framework)
examples/    # runnable reference apps / quickstarts
docs/        # architecture & usage documentation
```

## Status

This repository is in **Phase 1: The Foundation**. See [`todo.md`](./todo.md) for the
full roadmap and current progress.

- [x] Workspace + `tpt-erp-primitives` (`Money`, `Id`, currency markers) and `StateMachine` derive macro
- [x] `tpt-erp-macros`: `TptEntity` + `TptApi` derives (validation, audit, filter, Axum CRUD router)
- [x] `tpt-erp-entity` runtime traits + in-memory `Repository`
- [x] `tpt-erp-ledger` event store, double-entry rules, and CQRS projection engine
- [x] `tpt-erp-tenant` identification + Postgres RLS (Axum extractor/middleware behind the `axum` feature)
- [x] `tpt-erp-wasm` `wasmtime` sandbox (computation-only, fuel/memory limited, hot-swap)
- [x] `tpt-erp-bus` (in-memory + NATS JetStream) and `tpt-erp-cache` (in-memory + Redis/Dragonfly)
- [x] `tpt-erp-cli` plugin tooling and `tpt-erp-frontend` Leptos demo
- [ ] SQLx/Postgres repository backend for `tpt-erp-entity`
- [ ] Postgres-backed event store for `tpt-erp-ledger`
- [ ] Reference implementations (3PL/WMS, Manufacturing/MES)

## Quickstart

A developer should be able to spin up a type-safe CRUD API in under 10 minutes. The
[`examples/quickstart`](./examples/quickstart) crate demonstrates defining business
types with `tpt-erp-primitives` and the `StateMachine` macro.

```bash
cargo run -p quickstart
```

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](./LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](./LICENSE-MIT))

at your option.

Copyright © 2026 TPT Solutions.
