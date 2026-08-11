# Changelog

All notable changes to this project are documented in this file. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This repository is a workspace of framework crates (`crates/`) and reference ERP apps
(`examples/`). The `0.1.0` line covers the initial framework plus the six reference
implementations (3PL/WMS, Manufacturing/MES, Accounting/GL, E-commerce/OMS, Retail/POS,
Fleet/TMS) and the Phase 5 platform-hardening review.

## [Unreleased]

### Added
- `tpt-erp-observability` crate: structured `tracing` subscriber driven by `RUST_LOG`
  and a Prometheus `/metrics` endpoint, wired into the reference server.
- WASM plugin runtime now caps module size before compilation (`max_module_bytes`),
  ahead of the per-call fuel/memory limits.
- Generated `TptEntity` filters now support range (`*_min`/`*_max`) for numeric/date
  fields and substring/prefix (`*_contains`/`*_prefix`) for string fields, in addition
  to exact equality.
- Tenant RLS is now actually wired to a Postgres connection via the `sqlx` feature:
  `tenant_rls_middleware` opens a transaction and runs `SET LOCAL app.tenant_id` so
  Row-Level Security scopes every query for the request.
- CI now runs the `#[ignore]` stress/load tests in a dedicated job, and pins the
  declared MSRV.
- A dedicated CI workflow builds and WIT-validates the six WASM plugin guests.

### Changed
- Reconciled the declared MSRV across `Cargo.toml`, `rust-toolchain.toml`, and the
  `Dockerfile` (all `1.97`).

### Fixed
- Hardening pass (see `todo.md` Phase 5): RBAC principal is no longer clobbered by the
  generated router; the WASM wall-clock epoch ticker fires; OMS saga refund/await bugs
  fixed; WMS inventory floor check; NATS ack-after-process and wildcard consumer names;
  Redis connection no longer wrapped in a process-wide `Mutex`; `Money::allocate` is
  fallible; checked arithmetic helpers; double-entry balance enforced at apply time;
  GL period-close enforcement; POS pre-tax discount; and more.

## [0.1.0] - 2026-08-11

### Added
- Initial framework: `tpt-erp-primitives` (Money/Id/StateMachine), `tpt-erp-ledger`
  (event-sourced double-entry + CQRS), `tpt-erp-tenant` (Postgres RLS), `tpt-erp-wasm`
  (sandbox), `tpt-erp-bus`, `tpt-erp-cache`, `tpt-erp-macros` (TptEntity/TptApi),
  `tpt-erp-entity`, `tpt-erp-cli`, and `tpt-erp-frontend`.
- Six reference ERP implementations and their Leptos UIs.
