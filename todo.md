# TPT ERP RUST — Project Todo

## Phase 0: Project Setup & Licensing
- [x] git init
- [x] LICENSE-MIT and LICENSE-APACHE files (copyright TPT Solutions, 2026)
- [x] README.md (project overview / architecture summary)
- [x] CONTRIBUTING.md
- [x] .gitignore (Rust: /target, editor files)
- [x] rust-toolchain.toml
- [x] Base CI skeleton (fmt/clippy/test on PR) — GitHub Actions, sccache wired
- [x] Repo layout decision (crates/, examples/, docs/)
- [x] Initial commit (repo already has 3 foundational commits)

## Phase 1: The Foundation (Months 1-3)
### Workspace
- [x] Root Cargo.toml with [workspace] members
- [x] Skeleton crates: tpt-erp-primitives, tpt-erp-ledger, tpt-erp-macros, tpt-erp-wasm, tpt-erp-tenant
       (each with Cargo.toml `license = "MIT OR Apache-2.0"`)
- [x] Workspace-level lint config (clippy, deny warnings in CI via `-D warnings`)
- [x] cargo-chef Docker multi-stage build (target < 20MB image) — `Dockerfile` scaffold
- [x] sccache wired into CI (sccache-action + RUSTC_WRAPPER)

### tpt-erp-primitives
- [x] rust_decimal dependency; Currency marker types/trait
- [x] Money<C: Currency> wrapper: arithmetic ops, currency-mismatch prevented via generics,
       serde support, unit tests (precision/rounding/mismatch)
- [x] Id<T> strong-ID wrapper: storage (UUID/i64), PhantomData<T>, serde + sqlx impls,
       compile-fail tests (trybuild) proving cross-entity ID mixups don't compile
- [x] #[derive(StateMachine)] macro: attribute design, transition-checked enum codegen,
       example (Order: Draft->Confirmed->Shipped, no backward transitions), tests
- [x] Crate docs/examples

### tpt-erp-macros
- [x] Proc-macro scaffold (syn/quote/proc-macro2)
- [x] #[derive(StateMachine)]: transition-checked enum codegen, tests
- [x] #[derive(TptEntity)]: SQLx mapping decision (SQLx chosen over SeaORM — see
        note below), validation hook, audit fields (created/updated_at/by) + trait,
        generated all-optional query `Filter` + `ApplyFilter`, integration tests
- [x] #[derive(TptApi)]: Axum CRUD router (GET /, GET /:id, POST /, PUT /:id,
        DELETE /:id), pagination (folded into the `Filter`), filtering, RBAC hook
        (`AuthPolicy`), generated `IntoResponse` error type, integration tests.
        NOTE: GraphQL schema/resolvers (async-graphql) is **deferred** — the
        `Repository` trait isolates the storage backend so a GraphQL layer can be
        added later without touching entities.
- [x] "10-minute quickstart" example proving the Phase 1 milestone (see
        `examples/quickstart` — spins up a validated, audited, paginated,
        RBAC-guarded CRUD API with zero hand-written routes)
- [x] Macro usage docs (crates/tpt-erp-macros + crates/tpt-erp-entity)

### Frontend groundwork
- [x] Scaffold Leptos workspace member (`crates/tpt-erp-frontend`), basic Wasm build pipeline
        (SSR mode compiles on host; `trunk build` + `wasm32` target for the real WASM build)
- [x] Demo: share a `tpt-erp-primitives` type (`Money<Usd>`) between backend and
        Leptos frontend (`crates/tpt-erp-frontend/src/lib.rs` computes a line total
        with the same `Money` type the server uses)

> **Milestone**: a developer can spin up a type-safe, multi-tenant CRUD API in < 10 minutes.

## Phase 2: The Data & Ledger Core (Months 4-6)
### tpt-erp-ledger
- [x] Event schema (aggregate id, type, payload, timestamp, sequence)
- [x] Append-only event store with optimistic concurrency on append (in-memory reference;
       Postgres backend persists the same `StoredEvent` shape)
- [x] Double-Entry Core trait: balanced-transaction enforcement, tests (balanced/unbalanced)
- [x] CQRS projection engine: async `Projector` trait, `BalanceProjection` read-model,
       replay-from-scratch, projection-correctness tests

### tpt-erp-tenant
- [x] Tenant identification strategy (subdomain/header/JWT claim)
- [x] Postgres RLS policy templates + `SET LOCAL app.tenant_id` command builder
- [x] Connection middleware setting `SET LOCAL app.tenant_id` per request/transaction
- [x] Axum tenant-context extractor/middleware (`web` module, `axum` feature)
- [x] Cross-tenant isolation tests + negative/fuzz test suite (app-layer isolation + missing-tenant rejection)

### Supporting infra
- [x] Choose NATS JetStream vs Kafka for event processing/background jobs; integrate
      (decision: NATS JetStream — `crates/tpt-erp-bus`, in-memory + `nats` backend)
- [x] Redis/Dragonfly: session management (`crates/tpt-erp-cache` `SessionStore`,
      in-memory + `redis` backend)
- [x] Redis/Dragonfly: CQRS read-model cache layer (`crates/tpt-erp-cache` `ReadModelCache`,
      in-memory + `redis` backend)

### Integration
- [x] Axum + `tpt-erp-tenant` + `tpt-erp-ledger` server skeleton (`examples/server`, in-memory store)
- [x] End-to-end test: API transaction -> ledger entry -> tenant isolation verified

> **Milestone**: core processes financial transactions with 100% auditability and zero
> cross-tenant data leakage.

## Phase 3: The Wasm Boundary (Months 7-9)
### tpt-erp-wasm
- [x] wasmtime dependency + basic module loader
- [x] WIT host-guest contract: versioned "read ERP data" host functions, plugin-output types
       (see `crates/tpt-erp-wasm/wit/erp.wit`; `plugin` world imports only `erp`, never `wasi:*`)
- [x] Strict WASI host-binding layer: computation-only (no direct file I/O), fuel/memory limits
       (`RuntimeConfig`: fuel_per_call + max_memory_bytes enforced via wasmtime `consume_fuel` +
       `StoreLimits`; no WASI linked into the guest)
- [x] Sandbox safety tests: malicious/broken module can't crash host
       (host-binding translation, reject unknown imports, reject core-module, reject bad signature)
- [x] Hot-load/hot-swap mechanism (no host restart) — `PluginHandle::swap_module`

### CLI tool
- [x] `tpt plugin new`: scaffold a computation-only plugin template
       (`crates/tpt-erp-cli` → `tpt plugin new`; guest targets `wasm32-unknown-unknown`,
       NOT `wasm32-wasi`, so it is WASI-free and matches the computation-only contract)
- [x] Compile client Rust -> .wasm
       (`tpt plugin build` runs `cargo build --target wasm32-unknown-unknown --release`)
- [x] Validate compiled module against WIT contract before upload
       (`tpt plugin build` componentizes via `wit-component` + validates; `tpt plugin validate`
       loads it through `tpt-erp-wasm` and confirms it satisfies the `plugin` world)
- [x] Example plugin compiled and executed end-to-end
       (`examples/plugins/pricing` — a balance-tiered pricing engine that reads host data
       via `erp`; proven via `cargo test -p tpt-erp-cli --test e2e -- --ignored`)

> **Milestone**: proven ability to hot-load and execute custom business logic safely at
> runtime.

## Phase 4: Reference Implementations (Months 10-14)
### Sprint A: 3PL/WMS
- [x] Scaffold example app crate (`examples/wms`)
- [x] Real-Time Inventory Engine: event-stream-based inventory (via tpt-erp-ledger),
       sharded concurrent bin-location updates without a global row lock, optimistic
       concurrency, CQRS read-model replay + `tpt-erp-cache`, replenishment jobs on `tpt-erp-bus`,
       concurrency tests
- [x] Wave & Route Optimization: picker-path strategies (naive / nearest-neighbor /
       batch / S-shaped), distance comparison, benchmark vs naive (`#[ignore]` test)
- [x] IoT Ingestion: transport-agnostic high-throughput pipeline in `examples/wms/src/ingest.rs`
       (decode RFID/weight frames, back-pressured batching onto `tpt-erp-bus`, optional `mqtt`
       feature bridging `rumqttc`), load test (`#[ignore]`; thousands of msgs/sec)
- [x] Wasm routing plugins: `examples/plugins/routing` guest (Zone/Wave picking decision via
       host `erp` reads), componentizes + validates against the `plugin` world; dynamic per-client
       swap proven by `tpt-erp-wasm` `PluginHandle::swap_module` host test (`tests/swap.rs`)
- [x] Leptos UI: warehouse picker/operator view (`examples/wms-ui`)
- [ ] Engage logistics domain expert to validate business rules/workflows

### Sprint B: Manufacturing/MES
- [x] Scaffold example app crate (`examples/mes`)
- [x] Parallel MRP Engine: BOM tree model, rayon-based parallel explosion, shortage/lead-time
       calc, benchmark (4,000-part BOM; `cargo test -p mes --release -- --ignored`)
- [x] WIP State Machine: shop-floor item states via tpt-erp-primitives StateMachine
       (e.g. Machined -> Assembled), prerequisite-verification tests
- [x] Machine Telemetry & OEE: `examples/mes/src/oee.rs` (Availability×Performance×Quality)
       + `telemetry.rs` (CNC/PLC sample ingestion, `TelemetryStore` trait + in-memory impl,
       live OEE); TimescaleDB kept behind the `TelemetryStore` trait for later drop-in
- [x] Wasm QC plugins: `examples/plugins/qc` guest (QC tolerance check + telemetry parser),
       componentizes + validates against the `plugin` world; edge deploy = `swap_module`
       (no server restart), proven by `tpt-erp-wasm` swap test
- [x] Leptos UI: shop-floor operator view (WIP/QC entry) + OEE dashboard (`examples/mes-ui`)
- [ ] Engage manufacturing/plant domain expert to validate business rules/workflows

### Sprint C: Accounting/GL
- [x] Scaffold example app crate (`examples/gl`)
- [x] Multi-Currency Journal Engine: event-sourced double-entry posting (via
       tpt-erp-ledger), sharded concurrent per-account writes without a global row lock,
       optimistic concurrency, CQRS balance projection + `tpt-erp-cache`, concurrency tests
- [x] FX & Revaluation: explicit typed cross-currency conversion (`Money<From> ->
       Money<To>`), point-in-time rate table, period-end account revaluation
- [x] Period-End Close: StateMachine-derived close workflow (Open -> SoftClose ->
       Reconciling -> Closed -> Locked, reopen branch), trial-balance gate before Closed,
       generated closing/reversing entries, `gl.period_closed` job on `tpt-erp-bus`
- [x] Financial Reporting: TptEntity/TptApi chart of accounts, CQRS-replayed Trial
       Balance/Income Statement/Balance Sheet read models cached via `tpt-erp-cache`
- [x] Wasm tax plugin: `examples/plugins/tax` guest (jurisdiction tax-tier via host
       `erp` balance read), componentizes + validates against the `plugin` world
- [x] Leptos UI: journal-entry + trial-balance view (`examples/gl-ui`)
- [ ] Engage accounting/controller domain expert to validate business rules/workflows

### Sprint D: E-commerce/OMS
- [x] Scaffold example app crate (`examples/oms`)
- [x] TptEntity/TptApi Catalog: Product/Order CRUD with role-differentiated
        `AuthPolicy` (customer vs. staff), pagination/filtering/RBAC
- [x] Reservation Engine: event-sourced stock holds (sharded per SKU, TTL auto-release
        via `tpt-erp-cache`), oversell-prevention concurrency tests
- [x] Order Saga: Reserve -> Pay -> Fulfill -> Ship via StateMachine + hand-rolled
        compensating-transaction orchestrator on `tpt-erp-bus`; Pay step posts a real
        balanced transaction through `gl::journal`
- [x] Checkout: Axum wiring + Wasm promo plugin (per-SKU stock-aware discounting),
        `#[ignore]`d concurrent-checkout stress test proving zero oversell end-to-end
- [x] Wasm promo plugin: `examples/plugins/promo` guest, componentizes + validates
- [x] Leptos UI: storefront/checkout view with live saga-status badges (`examples/oms-ui`)
- [ ] Engage e-commerce/retail-ops domain expert to validate business rules/workflows

### Sprint E: Retail/POS
- [ ] Scaffold example app crate (`examples/pos`)
- [ ] Transaction State Machine: Cart -> Tendering -> Authorized -> Captured (void/
       refund branches), line items + tax in `Money<Usd>`
- [ ] Split Tender & Drawer Reconciliation: `Money::allocate`-based multi-tender
       splitting, expected-vs-counted cash-drawer math
- [ ] Offline-First Sync: local event-sourced transaction log, idempotent
       reconciliation replay to the central store on reconnect, `tpt-erp-cache`
       sync-checkpoint, `pos.synced` job on `tpt-erp-bus`
- [ ] Pricing plugin integration: `pos::pricing` gives `examples/plugins/pricing` a real
       backend home (balance-tiered discount via `tpt-erp-wasm`), hot-swap proven by
       `PluginHandle::swap_module` test
- [ ] Leptos UI: cashier terminal view with offline/online indicator (`examples/pos-ui`)
- [ ] Engage retail/store-ops domain expert to validate business rules/workflows

### Sprint F: Fleet/TMS
- [ ] Scaffold example app crate (`examples/tms`)
- [ ] GPS Telemetry Ingestion: transport-agnostic pipeline (decode GPS frames,
       back-pressured batching onto `tpt-erp-bus`, optional `mqtt` feature), load test
- [ ] Geofencing: point-in-polygon/circle containment + haversine distance, zone
       entry/exit events on `tpt-erp-bus`
- [ ] Route Optimization: nearest-neighbor + rayon-parallel 2-opt improvement,
       benchmark vs. naive (`#[ignore]` test), Wasm dispatch-plugin stop scoring
- [ ] Driver HOS State Machine: OffDuty/OnDuty/Driving/SleeperBerth via
       tpt-erp-primitives StateMachine + 11/14-hour rule-check layer, tests
- [ ] Wasm dispatch plugin: `examples/plugins/dispatch` guest, componentizes + validates
- [ ] Leptos UI: dispatcher live-map/route-plan view (`examples/tms-ui`)
- [ ] Engage fleet/logistics domain expert to validate business rules/workflows

### Shared infra
- [x] Kubernetes manifests/Helm chart for auto-scaling high-throughput ingestion nodes
       (`deploy/` chart: Deployment + HPA, NATS/Redis wiring, health probes)

### Workspace/docs housekeeping for Sprints C-F
- [x] Root `Cargo.toml`: add `examples/gl`, `examples/oms`, `examples/pos`, `examples/tms`
       and their `-ui` crates to `members`; promote `rayon` (and `rumqttc`'s pin) to
       `[workspace.dependencies]`
- [x] Root `README.md`: update the "Reference implementations" status bullet to list all
       six domains (3PL/WMS, Manufacturing/MES, Accounting/GL, E-commerce/OMS, Retail/POS,
       Fleet/TMS)

> **Milestone**: six production-ready, open-source reference ERPs (3PL/WMS,
> Manufacturing/MES, Accounting/GL, E-commerce/OMS, Retail/POS, Fleet/TMS) that serve as
> both marketing tools and stress-tests for the framework.
