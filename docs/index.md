# TPT ERP RUST

**Correctness by Compilation. Customization by Sandboxing. Scale by Default.**

TPT ERP RUST is a type-safe Rust framework for building domain-specific ERP systems.
It targets the three problems that most often sink ERP projects:

- **Data corruption.** Invalid states and cross-entity mistakes become compile errors.
  Double-entry ledger rules are enforced by the core, not by convention.
- **Customization forks.** Client-specific business logic runs as sandboxed WebAssembly
  plugins. They are hot-swappable, computation-only, and limited by fuel and memory caps,
  so you do not need a fork per customer.
- **Batch bottlenecks.** Async-first on tokio, with event sourcing and CQRS projections for
  high-throughput event streams.

## Where to start

- [Getting started](getting-started.md): define an entity and serve a CRUD API in minutes.
- [Architecture](architecture.md): how the framework crates fit together.
- [Operations and deployment](ops.md): Docker, Helm, and production configuration.
- [API reference](https://tpt-solutions.github.io/tpt-erp-rust/api/tpt_erp_primitives/index.html): rustdoc for every crate.

## Quick run

```bash
docker compose up --build
cargo install --path crates/tpt-erp-cli
export TOKEN=$(tpt token mint --tenant acme --roles admin --secret change-me-in-local-dev-only)
curl -s http://localhost:3000/balances -H "Authorization: Bearer $TOKEN"
```

## Reference implementations

Six reference verticals ship in `examples/`: 3PL/WMS, Manufacturing/MES, Accounting/GL,
E-commerce/OMS, Retail/POS, and Fleet/TMS. Each has a Leptos UI and a README.

## Links

- Source: [github.com/tpt-solutions/tpt-erp-rust](https://github.com/tpt-solutions/tpt-erp-rust)
- Crates: `tpt-erp-primitives`, `tpt-erp-macros`, `tpt-erp-entity`, `tpt-erp-ledger`,
  `tpt-erp-tenant`, `tpt-erp-wasm` on [crates.io](https://crates.io)
- License: MIT or Apache-2.0
