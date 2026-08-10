//! # tpt-bus
//!
//! Event-processing and background-job transport for TPT ERP.
//!
//! ## Backend decision: **NATS JetStream** (not Kafka)
//!
//! We standardize on NATS JetStream because it is the better fit for this
//! system's needs:
//!
//! - **Rust-native** — first-class [`async-nats`] client, no JVM.
//! - **Durable by default** — JetStream persists and replays events, which
//!   is exactly what an event-sourced ledger ([`tpt_ledger`]) requires for
//!   reprocessing/CQRS rebuilds.
//! - **Single binary** — one `nats-server` process (or a tiny cluster)
//!   covers both the event log *and* the background-job queue, shrinking
//!   the ops surface versus running Kafka + Zookeeper/KRaft.
//! - **Built-in flow control** — consumer groups, ack-based redelivery,
//!   and KV for dedup map cleanly onto background jobs.
//!
//! Kafka remains a viable alternative for very high fan-out analytics
//! workloads; if that becomes a requirement we can add a second backend
//! behind a `kafka` feature without touching the [`EventBus`]/[`JobQueue`]
//! contracts.
//!
//! The contracts here are object-safe traits so application code depends
//! only on the interface. [`memory`] is a fully-tested in-process
//! implementation; [`nats_impl`] (feature `nats`) is the JetStream backend.

use std::pin::Pin;

use futures::Stream;

pub mod memory;
#[cfg(feature = "nats")]
pub mod nats_impl;

/// A transported message.
#[derive(Debug, Clone)]
pub struct Message {
    /// Routing subject (e.g. `orders.created`, `jobs.invoice`).
    pub subject: String,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
}

/// A stream of [`Message`]s yielded by a subscription.
pub type MessageStream = Pin<Box<dyn Stream<Item = Message> + Send>>;

/// Errors raised by bus backends.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// The backend returned a transport/storage error.
    #[error("bus backend error: {0}")]
    Backend(String),
    /// A subscription could not be established.
    #[error("subscribe failed: {0}")]
    Subscribe(String),
}

/// Pub/sub transport for domain events.
///
/// Subjects use `.` separators (`orders.created`). Subscribers may use a
/// trailing `>` for prefix matching (`orders.>` matches `orders.created`).
#[async_trait::async_trait]
pub trait EventBus: Send + Sync {
    /// Publish an event to a subject.
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), BusError>;

    /// Subscribe to a subject, returning a stream of matching messages.
    /// The stream lives until dropped.
    async fn subscribe(&self, subject: &str) -> Result<MessageStream, BusError>;
}

/// Background-job queue, layered on top of the event bus.
///
/// Jobs are ordinary messages on `jobs.{type}` subjects, so the same
/// durability/redelivery guarantees apply.
#[async_trait::async_trait]
pub trait JobQueue: Send + Sync {
    /// Enqueue a job of `job_type` with the given payload.
    async fn enqueue(&self, job_type: &str, payload: &[u8]) -> Result<(), BusError>;

    /// Subscribe to jobs of `job_type`, returning a stream of messages.
    async fn subscribe_jobs(&self, job_type: &str) -> Result<MessageStream, BusError>;
}
