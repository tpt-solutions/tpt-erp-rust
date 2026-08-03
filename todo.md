# TPT ERP RUST — Project Todo

## Phase 0: Project Setup & Licensing
- [ ] git init
- [ ] LICENSE-MIT and LICENSE-APACHE files (copyright TPT Solutions, 2026)
- [ ] README.md (project overview / architecture summary)
- [ ] CONTRIBUTING.md
- [ ] .gitignore (Rust: /target, editor files)
- [ ] rust-toolchain.toml
- [ ] Base CI skeleton (fmt/clippy/test on PR)
- [ ] Repo layout decision (crates/, examples/, docs/)
- [ ] Initial commit

## Phase 1: The Foundation (Months 1-3)
### Workspace
- [ ] Root Cargo.toml with [workspace] members
- [ ] Skeleton crates: tpt-primitives, tpt-ledger, tpt-macros, tpt-wasm, tpt-tenant
      (each with Cargo.toml `license = "MIT OR Apache-2.0"`)
- [ ] Workspace-level lint config (clippy, deny warnings in CI)
- [ ] cargo-chef Docker multi-stage build (target < 20MB image)
- [ ] sccache wired into CI

### tpt-primitives
- [ ] rust_decimal dependency; Currency marker types/trait
- [ ] Money<C: Currency> wrapper: arithmetic ops, currency-mismatch prevented via generics,
      serde support, unit tests (precision/rounding/mismatch)
- [ ] Id<T> strong-ID wrapper: storage (UUID/i64), PhantomData<T>, serde + sqlx impls,
      compile-fail tests (trybuild) proving cross-entity ID mixups don't compile
- [ ] #[derive(StateMachine)] macro: attribute design, transition-checked enum codegen,
      example (Order: Draft->Confirmed->Shipped, no backward transitions), tests
- [ ] Crate docs/examples

### tpt-macros
- [ ] Proc-macro scaffold (syn/quote/proc-macro2)
- [ ] #[derive(TptEntity)]: SQLx/SeaORM mapping (decide + document which), validation hook,
      audit fields (created/updated_at/by) + trait, tests
- [ ] #[derive(TptApi)]: Axum CRUD router, pagination, filtering, RBAC hook,
      GraphQL schema/resolvers (e.g. async-graphql), integration tests
- [ ] "10-minute quickstart" example proving the Phase 1 milestone
- [ ] Macro usage docs

### Frontend groundwork
- [ ] Scaffold Leptos workspace member, basic Wasm build pipeline
- [ ] Demo: share a tpt-primitives type (Money or StateMachine) between backend and
      Leptos frontend via Wasm

> **Milestone**: a developer can spin up a type-safe, multi-tenant CRUD API in < 10 minutes.

## Phase 2: The Data & Ledger Core (Months 4-6)
### tpt-ledger
- [ ] Event schema (aggregate id, type, payload, timestamp, sequence)
- [ ] Append-only Postgres event store + optimistic concurrency on append
- [ ] Double-Entry Core trait: balanced-transaction enforcement, tests (balanced/unbalanced)
- [ ] CQRS projection engine: async projector trait, example read-model, replay-from-scratch,
      projection-correctness tests

### tpt-tenant
- [ ] Tenant identification strategy (subdomain/header/JWT claim)
- [ ] Postgres RLS policy templates
- [ ] Connection middleware setting `SET LOCAL app.tenant_id` per request/transaction
- [ ] Axum tenant-context extractor/middleware
- [ ] Cross-tenant isolation tests + negative/fuzz test suite

### Supporting infra
- [ ] Choose NATS JetStream vs Kafka for event processing/background jobs; integrate
- [ ] Redis/Dragonfly: session management
- [ ] Redis/Dragonfly: CQRS read-model cache layer

### Integration
- [ ] Axum + SQLx/SeaORM server skeleton using tpt-tenant + tpt-ledger
- [ ] End-to-end test: API transaction -> ledger entry -> tenant isolation verified

> **Milestone**: core processes financial transactions with 100% auditability and zero
> cross-tenant data leakage.

## Phase 3: The Wasm Boundary (Months 7-9)
### tpt-wasm
- [ ] wasmtime dependency + basic module loader
- [ ] WIT host-guest contract: versioned "read ERP data" host functions, plugin-output types
- [ ] Strict WASI host-binding layer: computation-only (no direct file I/O), fuel/memory limits
- [ ] Sandbox safety tests: malicious/broken module can't crash host
- [ ] Hot-load/hot-swap mechanism (no host restart)

### CLI tool
- [ ] `tpt plugin build`: scaffold plugin template (wasm32-wasi target)
- [ ] Compile client Rust -> .wasm
- [ ] Validate compiled module against WIT contract before upload
- [ ] Example plugin compiled and executed end-to-end

> **Milestone**: proven ability to hot-load and execute custom business logic safely at
> runtime.

## Phase 4: Reference Implementations (Months 10-14)
### Sprint A: 3PL/WMS
- [ ] Scaffold example app crate
- [ ] Real-Time Inventory Engine: event-stream-based inventory (via tpt-ledger),
      concurrent bin-location updates without row-locking, concurrency tests
- [ ] Wave & Route Optimization: picker path algorithm, benchmark vs naive/batch approach
- [ ] IoT Ingestion: MQTT integration (conveyor sensors/RFID gates), high-throughput
      pipeline, load test (thousands of msgs/sec)
- [ ] Wasm routing plugins: WIT contract for routing, example plugins (Zone Picking,
      Wave Picking, union break-routing rule), dynamic per-client plugin swap test
- [ ] Leptos UI: warehouse picker/operator view
- [ ] Engage logistics domain expert to validate business rules/workflows

### Sprint B: Manufacturing/MES
- [ ] Scaffold example app crate
- [ ] Parallel MRP Engine: BOM tree model, rayon-based parallel explosion, shortage/lead-time
      calc, benchmark (4,000-part BOM in milliseconds)
- [ ] WIP State Machine: shop-floor item states via tpt-primitives StateMachine
      (e.g. Machined -> Assembled), prerequisite-verification tests
- [ ] Machine Telemetry & OEE: CNC/PLC ingestion, TimescaleDB storage, real-time OEE calc
- [ ] Wasm QC plugins: WIT contract, example QC tolerance check, example telemetry parser,
      edge-node deploy test (microsecond evaluation, no server restart)
- [ ] Leptos UI: shop-floor operator view (WIP/QC entry) + OEE dashboard
- [ ] Engage manufacturing/plant domain expert to validate business rules/workflows

### Shared infra
- [ ] Kubernetes manifests/Helm chart for auto-scaling high-throughput ingestion nodes

> **Milestone**: two production-ready, open-source reference ERPs that serve as both
> marketing tools and stress-tests for the framework.
