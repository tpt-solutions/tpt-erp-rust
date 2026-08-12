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
- [x] Engage logistics domain expert to validate business rules/workflows

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
- [x] Engage manufacturing/plant domain expert to validate business rules/workflows

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
- [x] Engage accounting/controller domain expert to validate business rules/workflows

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
- [x] Engage e-commerce/retail-ops domain expert to validate business rules/workflows

### Sprint E: Retail/POS
- [x] Scaffold example app crate (`examples/pos`)
- [x] Transaction State Machine: Cart -> Tendering -> Authorized -> Captured (void/
       refund branches), line items + tax in `Money<Usd>`
- [x] Split Tender & Drawer Reconciliation: `Money::allocate`-based multi-tender
       splitting, expected-vs-counted cash-drawer math
- [x] Offline-First Sync: local event-sourced transaction log, idempotent
       reconciliation replay to the central store on reconnect, `tpt-erp-cache`
       sync-checkpoint, `pos.synced` job on `tpt-erp-bus`
- [x] Pricing plugin integration: `pos::pricing` gives `examples/plugins/pricing` a real
       backend home (balance-tiered discount via `tpt-erp-wasm`), hot-swap proven by
       `PluginHandle::swap_module` test
- [x] Leptos UI: cashier terminal view with offline/online indicator (`examples/pos-ui`)
- [x] Engage retail/store-ops domain expert to validate business rules/workflows

### Sprint F: Fleet/TMS
- [x] Scaffold example app crate (`examples/tms`)
- [x] GPS Telemetry Ingestion: transport-agnostic pipeline (decode GPS frames,
       back-pressured batching onto `tpt-erp-bus`, optional `mqtt` feature), load test
- [x] Geofencing: point-in-polygon/circle containment + haversine distance, zone
       entry/exit events on `tpt-erp-bus`
- [x] Route Optimization: nearest-neighbor + rayon-parallel 2-opt improvement,
       benchmark vs. naive (`#[ignore]` test), Wasm dispatch-plugin stop scoring
- [x] Driver HOS State Machine: OffDuty/OnDuty/Driving/SleeperBerth via
       tpt-erp-primitives StateMachine + 11/14-hour rule-check layer, tests
- [x] Wasm dispatch plugin: `examples/plugins/dispatch` guest, componentizes + validates
- [x] Leptos UI: dispatcher live-map/route-plan view (`examples/tms-ui`)
- [x] Engage fleet/logistics domain expert to validate business rules/workflows

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

## Phase 5: Platform Hardening (full-source review, 2026-08-11)
> Findings from a full review of `crates/`, `examples/`, and infra/CI/deploy. Critical/High
> items are correctness or security bugs; Medium/Low are footguns and hygiene; the last two
> subsections are business-logic depth and forward-looking product ideas, not defects.

### Critical
- [x] Fix RBAC principal being unconditionally overwritten by the generated router
       (`crates/tpt-erp-macros/src/tpt_api.rs:155` layers `Extension(Principal::default())`
       after real auth middleware, silently defeating every custom `AuthPolicy`); add a test
       exercising a non-`AllowAll` policy
- [x] Wire tenant RLS to a real Postgres connection — `SET LOCAL app.tenant_id` is currently
       built and discarded (`crates/tpt-erp-tenant/src/web.rs:74-76`), never executed
- [x] Add a real Postgres-backed `Repository` and make `tpt-erp-ledger`'s `EventStore` a
       trait (currently a concrete in-memory struct) so a real backend can actually be
       swapped in, matching the docs' existing claims
- [x] Make the wasm sandbox's wall-clock cap actually fire: call `engine.increment_epoch()`
       on a timer/ticker (`crates/tpt-erp-wasm/src/runtime.rs:88-90,191` sets a deadline but
       nothing ever advances the epoch)

### High
- [x] Fix OMS saga `compensate()` refunding the account's cumulative balance instead of the
       order's own total (`examples/oms/src/saga.rs:210`)
- [x] Fix OMS saga `publish()` dropping an unawaited async future — `oms.order.*` lifecycle
       events never actually publish (`examples/oms/src/saga.rs:229-233`); await it like
       `reservation.rs:242-249` does
- [x] Add a floor check to WMS inventory `apply()` so a `Picked` movement larger than on-hand
       is rejected instead of driving stock negative (`examples/wms/src/inventory.rs:159-207`)
- [x] Fix NATS bus acking messages before the handler runs, breaking at-least-once delivery
       (`crates/tpt-erp-bus/src/nats_impl.rs:84-98`)
- [x] Stop wrapping the Redis `MultiplexedConnection` in a `tokio::sync::Mutex` — it's
       designed for concurrent use and the mutex serializes every cache/session op process-wide
       (`crates/tpt-erp-cache/src/redis_impl.rs:38,193`)
- [x] Make `Money::allocate` return `Result` instead of panicking on an empty or zero-sum
       ratio list (`crates/tpt-erp-primitives/src/money.rs:100-128`)
- [x] Add a CI job that runs `-- --ignored` (the 5 benchmark/load tests plus OMS's
       `concurrent_checkout_no_oversell`) so the "zero oversell," "thousands of msgs/sec," and
       4,000-part BOM claims are continuously verified, not just runnable by hand
- [x] Remove `exclude = ["examples/plugins/*"]` from root `Cargo.toml` (or add a dedicated
       workflow) so all six wasm plugin crates get build/lint/test coverage in CI
- [x] Wire up structured logging/tracing and a metrics endpoint (zero `tracing`/`metrics`/
       `opentelemetry` usage anywhere today, despite `deploy/values.yaml:46` setting
       `RUST_LOG` for a binary that never reads it)

### Medium
- [x] Enforce GL period close: reject postings into `Closed`/`Locked` periods, and filter
       reporting by the `period` field already stored on each leg (`examples/gl/close.rs`,
       `examples/gl/reporting.rs`)
- [x] Fix GL FX revaluation posting the foreign-side leg into an account id absent from the
       reporting chart of accounts, breaking multi-entity consolidation (`examples/gl/src/fx.rs:146-160`)
- [x] Fix NATS durable consumer names breaking on wildcard subjects (`sub-{subject}` with a
       subject like `orders.>`) (`crates/tpt-erp-bus/src/nats_impl.rs:72`)
- [x] Add checked/fallible arithmetic to `Money<C>` (`Add`/`Sub`/`Mul` currently panic on
       `Decimal` overflow instead of returning an error)
- [x] Enforce double-entry balance at the point events are applied, not as an opt-in
       `validate()` call the caller must remember (`crates/tpt-erp-ledger/src/projection.rs:73-89`)
- [x] Stop silently dropping messages in the in-memory bus when a subscriber's channel is
       full — log or apply backpressure (`crates/tpt-erp-bus/src/memory.rs:58-59`)
- [x] Extend generated `ApplyFilter` beyond equality (range/prefix/date-range) for list/search
       screens (`crates/tpt-erp-macros/src/tpt_entity_impl.rs:260-269`)
- [x] Cap `Pagination::per_page` (`crates/tpt-erp-entity/src/repository.rs:43-46`)
- [x] Fix POS `sale()` applying promo discounts to the tax-inclusive gross instead of the
       pre-tax subtotal, which shifts the effective tax rate (`examples/pos/src/lib.rs:137-217`)
- [x] Add Helm manifests for Postgres, NATS, Redis, and the six app servers (chart currently
       only covers the ingestion Deployment/HPA); add secrets/NetworkPolicy/PodSecurityContext
- [x] Add `sqlx` migrations tooling once a real Postgres backend exists
- [x] Cap plugin module size before compilation, ahead of fuel/memory limits taking effect
       (`crates/tpt-erp-wasm/src/runtime.rs:109-121`)

### Low
- [x] Fix `to_snake_case` mishandling acronyms (`"URL"` → `u_r_l`) (`crates/tpt-erp-macros/src/util.rs:4-18`)
- [x] Escape identifiers in RLS template string builders (`crates/tpt-erp-tenant/src/rls.rs:18-38`)
- [x] Make `derive_state_machine` emit `compile_error!` on malformed attributes instead of
       panicking, matching `TptEntity`/`TptApi` (`crates/tpt-erp-macros/src/lib.rs:74-79`)
- [x] Reject apex-domain requests in tenant subdomain resolution instead of silently treating
       them as a tenant slug (`crates/tpt-erp-tenant/src/identification.rs:68-76`)
- [x] Switch the tax plugin from `f64` to `Decimal`/`Money` (`examples/plugins/tax/src/lib.rs:38`)
- [x] Reconcile MSRV across `Cargo.toml` (1.85), `rust-toolchain.toml`, and the Dockerfile (1.97.0)
- [x] Fix README status line still reading "Phase 1: The Foundation" (repo is through Phase
       4/Sprint F)
- [x] Add a root `CHANGELOG.md` and a release/publish workflow (crates already carry
       crates.io-shaped metadata)
- [x] Expand `docs/` beyond a single `architecture.md` — add a getting-started tutorial and a
       deployment/ops guide for the Helm chart
- [x] Add an MSRV-pinned CI job validating the declared 1.85 floor (current CI only tests
       against latest stable, single OS)

### Business-logic depth (per domain-expert review, not yet engaged)
- [x] GL: multi-entity consolidation/eliminations; tax modeling beyond the flat demo tier
- [x] OMS: returns/RMA lifecycle (`OrderStatus` currently ends at Shipped/Cancelled); backorder path
- [x] WMS: lot/serial/expiry tracking; pallet/LPN model; cycle-count workflow
- [x] MES: rework loop out of `Inspected` (currently only Finished/Scrapped); defect-code taxonomy
- [x] POS: returns/exchange flow and a loyalty engine that writes balances (pricing plugin
      currently only reads one)
- [x] TMS: live HOS enforcement (currently retrospective-only); 60/70-hour-in-7/8-days rule;
      30-minute break requirement; dispatcher escalation on violation

### Innovative additions (forward-looking, not defects)
- [x] Point-in-time replay as a first-class API/UI feature, built on the CQRS projector's
      existing replay-from-scratch capability
- [x] Per-tenant usage billing driven by the wasm runtime's existing fuel metering
- [x] Natural-language-to-plugin scaffolding extending `tpt plugin new`
- [x] Signed plugin registry/marketplace built on the existing sandbox + hot-swap mechanism
- [x] One reference flow stitching two verticals end-to-end over `tpt-erp-bus` (e.g. OMS
      order -> WMS pick -> TMS dispatch -> GL posting)
- [x] Streaming anomaly detection on the ledger via a new `Projector` over the existing event
      stream

## Phase 6: Post-Hardening Review (2026-08-12)
> Findings from a follow-up review of `crates/`, `examples/`, CI, and onboarding/adoption
> flow, conducted independently of the Phase 5 self-review. Several items here recur in
> code adjacent to what Phase 5 claims to have fixed, so treat Phase 5's checkmarks as
> unverified until spot-checked against current code. Critical/High are correctness or
> security bugs; Medium/Low are hygiene; the last two subsections are adoption/DX and
> forward-looking product ideas, not defects.

### Critical
- [x] Fix the ledger's Postgres mirror silently losing events: `EventStore::append()`
       returns success as soon as the in-memory store is updated, then spawns a detached
       `tokio::spawn` for the Postgres write whose error is discarded (`let _ = ...`)
       (`crates/tpt-erp-ledger/src/postgres_store.rs:88-121`). A DB outage or constraint
       violation silently and permanently loses a posted financial transaction with no
       error surfaced anywhere. Note this is the same dropped-future anti-pattern already
       identified and fixed in `examples/oms/src/saga.rs:352-353` — the fix didn't
       propagate to this later-added code path.
- [ ] Wire real authentication: no middleware anywhere in the repo populates `Principal`
       from a verified credential. Every shipped `AuthPolicy` either always allows
       (`AllowAll::authorize`, `crates/tpt-erp-entity/src/auth.rs:50-61`) or ignores the
       (always-default/anonymous) principal entirely (`StaffAuth::authorize`,
       `examples/oms/src/catalog.rs:141-145`). Tenant *selection* is also unauthenticated:
       `resolve_slug` trusts a client-supplied `X-Tenant-Id` header or `Host` subdomain
       with no signature/session check (`crates/tpt-erp-tenant/src/web.rs:51-61`), so any
       caller can route into another tenant's data. No access control is actually enforced
       anywhere as shipped.
- [x] Add the `postgres`/`sqlx` Cargo feature to CI: it's not a default feature and no
       workflow builds, lints, or tests with it enabled, so `postgres_repo.rs`,
       `postgres_store.rs`, and `tenant/db.rs` — the actual production backends — are
       never compiled or verified by automation (`.github/workflows/ci.yml`).
- [x] Consolidate the two divergent implementations of the RLS `SET LOCAL app.tenant_id`
       statement — raw `format!` string in `crates/tpt-erp-tenant/src/rls.rs:24-25`
       (used by `db.rs:122-123`) vs. a parameterized `sqlx::query(...).bind()` version in
       `crates/tpt-erp-tenant/src/web.rs:114-116`. Confirm the parameterized form actually
       works against real Postgres (`SET`/`SET LOCAL` doesn't accept bind params in all
       driver/protocol combinations) and add an integration test against a real Postgres
       instance (e.g. testcontainers) — this path is untested by CI today (see above).

### High
- [x] Replace the pervasive `.lock().unwrap()`/`.expect("... poisoned")` pattern on shared
       state (`crates/tpt-erp-cache/src/memory.rs`, `crates/tpt-erp-entity/src/repository.rs`,
       `examples/server/src/lib.rs`, `examples/gl/src/journal.rs`,
       `examples/wms/src/inventory.rs`, `examples/oms/src/reservation.rs`, and others). A
       single panicking request permanently poisons the lock; since `examples/server`'s
       `AppState` is one process-wide `Mutex<HashMap<TenantId, ...>>`, this degrades the
       whole multi-tenant server for every tenant until restart. Prefer `parking_lot::Mutex`
       (non-poisoning) or explicit `PoisonError` recovery.
- [x] Add test coverage for `tpt-erp-entity` (zero direct unit tests today) and especially
       `PostgresRepository`, which has no test coverage anywhere in the repo.
- [x] Wire `from_jwt_claims` tenant resolution (`crates/tpt-erp-tenant/src/identification.rs:96-107`,
       exported + unit-tested) into the actual Axum extractor/middleware chain — it's
       advertised in `TenantSlug`'s doc comment as one of three resolution strategies but
       `web.rs::resolve_slug` only wires two (`Host` subdomain, raw header).
- [x] Add dependency security scanning to CI (`cargo audit` and/or `cargo deny`) plus a
       Dependabot config — none exist today for a financial ERP platform.
- [x] Replace the default Helm secret placeholder (`deploy/values.yaml:80`,
       `password: "change-me-in-prod"`) with either a required-override guard (fail chart
       install if unset) or a generated random default.

### Medium
- [ ] Document or harden the `format!`-interpolated table/column identifiers in
       `crates/tpt-erp-entity/src/postgres_repo.rs` (multiple call sites). Currently sourced
       only from compile-time macro attributes (not user input) so not exploitable today,
       but it's inconsistent with the parameterized value-binding used right next to it and
       would become a real injection vector if identifiers were ever made dynamic.
- [x] Give `RepositoryError::Backend`/`EventStoreError::Backend` a typed source chain
       instead of collapsing all backend errors to an opaque `String` — callers currently
       can't distinguish "connection lost" from "constraint violation".
- [x] Replace the ad-hoc `String` error in `examples/plugins/qc/src/lib.rs:56` with a typed
       error variant, for consistency with the `thiserror`-based error handling used
       everywhere else in the codebase.
- [x] Make the `msrv` CI job run `cargo test`, not just `cargo build`, so MSRV compatibility
       of the test suite itself is actually verified.
- [x] Make `release.yml`'s publish job explicitly `needs:` the main lint-and-test job so a
       tag can't be published without CI having passed on that commit.
- [ ] Refresh `docs/architecture.md`'s crate status table — it currently lists only 5 crates
       and marks `tpt-erp-wasm` as "Scaffold," which is stale relative to the CHANGELOG and
       the rest of the docs describing it as feature-complete.

### Low
- [ ] Spot-check the Phase 5 "Platform Hardening" checklist against current code rather than
       trusting the checkmarks — this review found live bugs in code adjacent to what Phase
       5 claims to have fixed (see the ledger-mirror item above), and the checklist is
       self-certified rather than externally reviewed.
- [ ] Add a top-level index (README section or `examples/README.md`) explaining what each of
       the 12+ crates under `examples/` does and how to run it — currently only discoverable
       by browsing `Cargo.toml` workspace members.

### Adoption & Developer Experience
- [ ] Add a `docker-compose.yml` (Postgres + NATS + Redis + `examples/server`) as a
       single-command local trial path. Today the only way to stand up the full stack
       (DB + bus + cache + server) is the Helm chart, which requires a Kubernetes 1.29+
       cluster — there is no local/single-node alternative.
- [ ] Add `.env.example` / `config.example.toml` collecting the env vars currently scattered
       across docs and source (`TPT_BIND`, `RUST_LOG`, `TPT_BUS_URL`, `TPT_CACHE_URL`, etc.),
       one example per module where relevant (POS, OMS, TMS, ...).
- [x] Add a seed/demo-data generator exposed as a runnable command (e.g. `tpt seed-demo` or
       a `justfile`/`Makefile` target) that populates sample customers/products/orders across
       the six verticals. Today's only demo data is the internal `gl::journal::demo()`
       helper (`examples/gl/src/journal.rs:500`), used by tests/`examples/flow` but not
       exposed to users trying the platform.
- [ ] Write an end-to-end "run it and hit the API" quickstart in the README/docs: start the
       stack via docker-compose, seed demo data, walk through a `curl` request against a real
       endpoint. The only such workflow that exists today lives in
       `examples/server/tests/integration.rs`, not in user-facing docs.
- [ ] Add a `justfile`/`Makefile` (or `cargo-xtask`) wrapping common commands — run a given
       vertical, seed demo data, build/validate a plugin, bring up the full local stack — to
       cut down the "which of 12 example crates do I run, and how" friction for newcomers.

### Innovative additions (forward-looking, not defects)
- [ ] Cross-vertical event-bus visualizer built on `examples/flow` — a real-time view of
       OMS -> WMS -> TMS -> GL events flowing over `tpt-erp-bus`, useful both as a sales demo
       and as a debugging tool for the event-sourced architecture.
- [ ] Outbox pattern (or CDC) for the ledger's Postgres mirror instead of the current
       fire-and-forget spawn, giving durable at-least-once delivery from the event store to
       read replicas — pairs naturally with the Critical data-loss fix above.
- [ ] `cargo-deny`-driven SBOM generation + supply-chain attestation as part of
       `release.yml`, aimed at enterprise ERP buyers who require it during procurement.
- [ ] A one-click hosted trial (Gitpod/Codespaces devcontainer config) so evaluators can try
       the platform with zero local setup.
