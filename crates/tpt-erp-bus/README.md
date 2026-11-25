# tpt-erp-bus

> Event-processing and background-job transport for TPT ERP.

`tpt-erp-bus` is the messaging layer that decouples domain events from the
consumers (projections, job workers) that react to them. Application code depends
only on two object-safe trait contracts; concrete backends are selected by
feature flag.

## Backend decision: PostgreSQL

The durable backend is our own implementation on **PostgreSQL**, the database the
ledger already runs on:

- **No new infrastructure.** Events and jobs live in the same database as ledger state,
  so they share backups and operational tooling.
- **No third-party messaging dependency.** Keeps the dependency tree free of
  Apache-only licensed transport crates (see `deny.toml`).
- **Event-sourcing friendly.** Events are stored in order and can be replayed for CQRS
  rebuilds ([`tpt-erp-ledger`](../tpt-erp-ledger/README.md)).

Guarantees (see [`postgres_impl`](src/postgres_impl.rs)): publishes are serialized so
events become visible in order; each subject has a durable cursor that only advances past
acknowledged messages (at-least-once across restarts); every subscription receives every
matching message (fan-out). Competing consumers for one job type are not load-balanced yet.

## Contracts

### `EventBus`

Pub/sub for domain events. Subjects use `.` separators (`orders.created`);
subscribers may use a trailing `>` for prefix matching (`orders.>`).

```rust
use tpt_erp_bus::{EventBus, InMemoryBus};

let bus = InMemoryBus::new();
let mut sub = bus.subscribe("orders.>").await?;
bus.publish("orders.created", b"evt-1").await?;
```

### `JobQueue`

Background jobs layered on top of the bus: jobs are ordinary messages on
`jobs.{type}` subjects, so the same durability/redelivery guarantees apply.

```rust
use tpt_erp_bus::{JobQueue, InMemoryBus};

let bus = InMemoryBus::new();
let mut worker = bus.subscribe_jobs("invoice").await?;
bus.enqueue("invoice", b"job-42").await?;
```

## Backends

| Backend | Feature | Notes |
|---------|---------|-------|
| `InMemoryBus` ([`memory`](src/memory.rs)) | default (always on) | `tokio::sync::mpsc`; exact + `>`-prefix matching. No external service. |
| `PostgresBus` ([`postgres_impl`](src/postgres_impl.rs)) | `postgres` | Durable events in `tpt_bus_messages`; per-subject consumer cursors in `tpt_bus_consumers`; ordered publish; at-least-once with ack-based cursors. Uses the ledger's Postgres database. |

```toml
tpt-erp-bus = { features = ["postgres"] }
```

```rust
let bus = tpt_erp_bus::postgres_impl::PostgresBus::connect("postgres://tpt:…@localhost/tpt").await?;
```

## Status

Early development (0.1.0). The in-memory implementation is fully tested. The PostgreSQL
backend (`postgres` feature) has integration tests that need a live database, run with
`TPT_BUS_TEST_DATABASE_URL=... cargo test -p tpt-erp-bus --features postgres -- --ignored`.
APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
