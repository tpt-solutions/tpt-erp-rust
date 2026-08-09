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

| Crate           | Responsibility                                                          | Status      |
|-----------------|------------------------------------------------------------------------|-------------|
| `tpt-primitives`| `Money<C>`, `Id<T>`, `IntId<T>`, `Currency`, `StateMachine` derive.    | Implemented |
| `tpt-macros`    | Proc-macros. `StateMachine` implemented; `TptEntity`/`TptApi` planned. | Partial     |
| `tpt-ledger`    | Append-only event store (optimistic concurrency), double-entry core, CQRS projections. | Implemented (core) |
| `tpt-tenant`    | Multi-tenancy: identification + Postgres RLS + `SET LOCAL` + Axum extractor/middleware. | Implemented  |
| `tpt-wasm`      | `wasmtime` sandbox for safe, hot-loadable business-logic plugins.      | Scaffold    |

## Design principles

- **Correctness by compilation.** Invalid states (cross-currency math, cross-entity
  id mixups, illegal state transitions) are unrepresentable or rejected at compile time.
- **Customization by sandboxing.** Client logic runs as Wasm modules with fuel/memory
  limits and a strict host contract — no forks, no host crashes.
- **Scale by default.** Async-first (tokio), event sourcing, and parallelism-ready
  data structures.

See the root [`README.md`](../README.md) and [`todo.md`](../todo.md) for the roadmap.
