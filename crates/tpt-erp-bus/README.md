# tpt-erp-bus

> Event-processing and background-job transport for TPT ERP.

`tpt-erp-bus` is the messaging layer that decouples domain events from the
consumers (projections, job workers) that react to them. Application code depends
only on two object-safe trait contracts; concrete backends are selected by
feature flag.

## Backend decision: NATS JetStream (not Kafka)

The workspace standardized on **NATS JetStream** because it fits this system best:

- **Rust-native** — first-class [`async-nats`] client, no JVM.
- **Durable by default** — JetStream persists and replays events, exactly what an
  event-sourced ledger ([`tpt-erp-ledger`](../tpt-erp-ledger/README.md)) needs for
  reprocessing and CQRS rebuilds.
- **Single binary** — one `nats-server` process covers both the event log *and*
  the background-job queue, shrinking the ops surface vs. Kafka + Zookeeper/KRaft.
- **Built-in flow control** — consumer groups, ack-based redelivery, and KV for
  dedup map cleanly onto background jobs.

Kafka remains viable for very high fan-out analytics; if needed, a second backend
can be added behind a `kafka` feature without touching the contracts.

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
| `NatsBus` ([`nats_impl`](src/nats_impl.rs)) | `nats` | Connects to a JetStream server; durable `tpt-events` stream, `>` catch-all subject, pull consumers with explicit acks. |

```toml
tpt-erp-bus = { features = ["nats"] }
```

```rust
let bus = tpt_erp_bus::NatsBus::connect("nats://localhost:4222").await?;
```

## Status

Early development (0.1.0). The in-memory implementation is fully tested; the
NATS backend is feature-gated. APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
