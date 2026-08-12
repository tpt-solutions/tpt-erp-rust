//! Append-only event schema shared across the ledger.
//!
//! Events are immutable facts. Each [`StoredEvent`] is addressed to an aggregate and
//! carries a monotonically increasing `sequence` within that aggregate, which the store
//! uses for optimistic-concurrency control.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Error type for event-store operations.
#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error(
        "optimistic concurrency conflict for aggregate: expected version {expected}, but current is {current}"
    )]
    Conflict { expected: u64, current: u64 },
    #[error("failed to serialize event payload: {0}")]
    Serialize(#[from] serde_json::Error),
    /// A backend (e.g. Postgres) error.
    ///
    /// The original error is preserved as the [`std::error::Error::source`], so callers
    /// can downcast to distinguish failure modes (e.g. connection loss from a constraint
    /// violation) rather than only seeing a flattened string.
    #[error("event store backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A domain event ready to be appended. The `payload` is any serializable value; it is
/// stored as JSON so the log is schema-flexible while remaining strongly typed at read time.
#[derive(Debug, Clone)]
pub struct Event<A> {
    pub aggregate_id: A,
    pub event_type: String,
    pub payload: Value,
}

impl<A> Event<A> {
    /// Build an event from a serializable payload.
    pub fn new(
        aggregate_id: A,
        event_type: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<Self, EventStoreError> {
        Ok(Self {
            aggregate_id,
            event_type: event_type.into(),
            payload: serde_json::to_value(payload)?,
        })
    }
}

/// An event that has been durably appended to the log.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent<A> {
    pub aggregate_id: A,
    pub sequence: u64,
    pub event_type: String,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
}

impl<A> StoredEvent<A> {
    /// Attempt to deserialize the JSON `payload` back into a typed domain event.
    pub fn typed_payload<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.payload.clone())
    }
}
