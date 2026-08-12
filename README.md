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

This repository is in **Phase 6: Post-Hardening Review** — all six reference ERPs
(3PL/WMS, Manufacturing/MES, Accounting/GL, E-commerce/OMS, Retail/POS, Fleet/TMS) are
implemented, the framework core has been through a full-source security/correctness
review (Phases 5 & 6), and the remaining hardening items have been resolved. See
[`todo.md`](./todo.md) for the roadmap and the review's resolved items.

- [x] Workspace + `tpt-erp-primitives` (`Money`, `Id`, currency markers) and `StateMachine` derive macro
- [x] `tpt-erp-macros`: `TptEntity` + `TptApi` derives (validation, audit, filter, Axum CRUD router, RBAC)
- [x] `tpt-erp-entity` runtime traits + in-memory **and Postgres (`sqlx`)** `Repository`
- [x] `tpt-erp-ledger` event store (in-memory + Postgres mirror), double-entry rules, CQRS projection engine
- [x] `tpt-erp-tenant` identification + Postgres RLS + **real JWT (HS256) auth** (Axum middleware behind the `auth` feature)
- [x] `tpt-erp-wasm` `wasmtime` sandbox (computation-only, fuel/memory limited, hot-swap)
- [x] `tpt-erp-bus` (in-memory + NATS JetStream) and `tpt-erp-cache` (in-memory + Redis/Dragonfly)
- [x] `tpt-erp-cli` plugin tooling (`plugin new|build|validate|run`, `seed-demo`, `token`) and `tpt-erp-frontend` Leptos demo
- [x] Reference implementations: 3PL/WMS, Manufacturing/MES, Accounting/GL, E-commerce/OMS, Retail/POS, Fleet/TMS

## Quickstart

A developer should be able to spin up a type-safe CRUD API in under 10 minutes. The
[`examples/quickstart`](./examples/quickstart) crate demonstrates defining business
types with `tpt-erp-primitives` and the `StateMachine` macro.

```bash
cargo run -p quickstart
```

### Run it and hit the API

The fastest way to stand up the full stack (Postgres + NATS + Redis + the reference
ledger server) is the bundled `docker-compose.yml`:

```bash
# 1. Start Postgres, NATS, Redis, and the server.
docker compose up --build

# 2. Mint a dev JWT scoped to tenant `acme` (uses the secret from docker-compose).
export TOKEN=$(tpt token mint --tenant acme --roles admin \
  --secret change-me-in-local-dev-only)

# 3. Post a balanced double-entry transaction.
curl -s -X POST http://localhost:3000/transactions \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"entries":[
        {"account":"11111111-1111-1111-1111-111111111111","side":"debit","amount":"100.00"},
        {"account":"22222222-2222-2222-2222-222222222222","side":"credit","amount":"100.00"}
      ]}'

# 4. Read the tenant's balances (cross-tenant data is structurally isolated).
curl -s http://localhost:3000/balances -H "Authorization: Bearer $TOKEN"
```

Without Docker, run the server directly (auth is enforced when `TPT_JWT_SECRET` is
set; otherwise it falls back to dev-only header/subdomain tenant resolution):

```bash
TPT_JWT_SECRET=dev-secret cargo run -p server
# mint a token:  tpt token mint --secret dev-secret --tenant acme
```

See [`examples/README.md`](./examples/README.md) for the full index of reference
verticals, UIs, and Wasm plugins, and [`docs/architecture.md`](./docs/architecture.md)
for the crate-level design.

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](./LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](./LICENSE-MIT))

at your option.

Copyright © 2026 TPT Solutions.
