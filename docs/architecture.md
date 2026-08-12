# Architecture

TPT ERP RUST is a workspace of cohesive, loosely coupled crates. This document
summarizes the layout and the responsibility of each core library.

## Layout

```
crates/      framework library crates
examples/   runnable reference apps / quickstarts
docs/       architecture & usage notes
```

## Crates

| Crate                 | Responsibility                                                                 | Status      |
|-----------------------|-------------------------------------------------------------------------------|-------------|
| `tpt-erp-primitives`  | `Money<C>`, `Id<T>`, `IntId<T>`, `Currency`, `StateMachine` derive, `Money::allocate`. | Implemented |
| `tpt-erp-macros`      | Proc-macros: `StateMachine`, `TptEntity`, `TptApi` (CRUD router + RBAC).     | Implemented |
| `tpt-erp-ledger`      | Append-only event store (optimistic concurrency), double-entry core, CQRS projections, Postgres mirror (`EventStore` trait). | Implemented |
| `tpt-erp-entity`      | `Repository` trait, `PostgresRepository`, in-memory repo, validation, audit, `AuthPolicy`/`Principal`, filter/pagination. | Implemented |
| `tpt-erp-tenant`      | Multi-tenancy: identification, Postgres RLS + `SET LOCAL`, Axum extractor/middleware, **real JWT (HS256) auth** (`auth` module). | Implemented |
| `tpt-erp-wasm`        | `wasmtime` sandbox for safe, hot-loadable, computation-only business-logic plugins (WIT contract, fuel/memory limits, hot-swap). | Implemented |
| `tpt-erp-cache`       | `SessionStore` + `ReadModelCache` over in-memory or Redis backends.          | Implemented |
| `tpt-erp-bus`         | Event bus (`tpt-erp-bus`) over in-memory or NATS JetStream backends.         | Implemented |
| `tpt-erp-observability`| Structured tracing/logging + metrics endpoint wiring.                       | Implemented |
| `tpt-erp-cli`         | `tpt` CLI: `plugin new|build|validate|run`, `seed-demo`, `token mint`.      | Implemented |
| `tpt-erp-frontend`    | Leptos workspace member; shares `Money<Usd>` with the backend.              | Implemented |


## Design principles

- **Correctness by compilation.** Invalid states (cross-currency math, cross-entity
  id mixups, illegal state transitions) are unrepresentable or rejected at compile time.
- **Customization by sandboxing.** Client logic runs as Wasm modules with fuel/memory
  limits and a strict host contract — no forks, no host crashes.
- **Scale by default.** Async-first (tokio), event sourcing, and parallelism-ready
  data structures.

See the root [`README.md`](../README.md) and [`todo.md`](../todo.md) for the roadmap.
