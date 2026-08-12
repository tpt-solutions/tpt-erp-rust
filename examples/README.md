# Examples

Runnable reference apps and quickstarts built on the `tpt-erp-*` framework crates.
Each is a workspace member (see the root `Cargo.toml`). Most reference verticals ship
a headless domain crate plus a `*-ui` Leptos frontend.

## Quickstarts

| Crate         | What it shows |
|---------------|---------------|
| `quickstart`  | 10-minute quickstart: a type-safe, multi-tenant CRUD API from `tpt-erp-primitives` + `TptEntity` + `TptApi` (zero hand-written routes). |
| `server`      | Reference Axum server wiring `tpt-erp-tenant` (multi-tenancy + JWT auth) with `tpt-erp-ledger` (append-only event store, double-entry core). Run with `TPT_JWT_SECRET` to enforce real authentication. |

## Reference verticals

| Domain        | Headless crate | UI crate   | Description |
|---------------|----------------|------------|-------------|
| 3PL / WMS     | `wms`          | `wms-ui`   | Real-time, event-sourced inventory engine + wave/route optimization + IoT ingestion + Wasm routing plugin. |
| Manufacturing | `mes`          | `mes-ui`   | Parallel MRP engine + WIP state machine + machine telemetry/OEE + Wasm QC plugin. |
| Accounting    | `gl`           | `gl-ui`    | Multi-currency event-sourced double-entry journal, FX revaluation, period-end close, CQRS financial reporting + Wasm tax plugin. |
| E-commerce    | `oms`          | `oms-ui`   | Catalog CRUD (role-differentiated `AuthPolicy`), event-sourced reservations, order saga (reserve→pay→fulfill→ship), Wasm promo plugin, Leptos storefront. |
| Retail / POS  | `pos`          | `pos-ui`   | Transaction state machine, split tender + drawer reconciliation, offline-first sync, Wasm pricing plugin. |
| Fleet / TMS   | `tms`          | `tms-ui`   | GPS ingestion, geofencing, route optimization, driver HOS state machine, Wasm dispatch plugin. |

## Cross-cutting

| Crate   | What it shows |
|---------|---------------|
| `flow`  | Cross-vertical reference flow: an OMS order triggers a WMS pick → TMS dispatch → GL posting, orchestrated entirely over `tpt-erp-bus`. |

## Wasm plugins

The `examples/plugins/*` crates are **excluded** from the workspace (they target
`wasm32-unknown-unknown` and are componentized via `wit-component`). Build and validate
them with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/pricing
tpt plugin validate examples/plugins/pricing/target/wasm32-unknown-unknown/release/*.wasm
```

Available plugin crates: `pricing`, `routing`, `qc`, `tax`, `promo`, `dispatch`.

## Running an example

```bash
# Headless domain crate (e.g. the GL reference implementation's demo + reporting):
cargo run -p gl --bin gl -- --help

# A Leptos UI (served via its own binary / `trunk` for the wasm frontend):
cargo run -p gl-ui
```

See the root [`README.md`](../README.md) for the full platform overview and the
local-trial quickstart (docker-compose + `tpt seed-demo` + authenticated `curl`).
