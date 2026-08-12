//! Outbox pattern for durable, at-least-once delivery from the ledger's event store to the
//! event bus (and thus to CQRS read models / downstream consumers).
//!
//! The naive approach — `append()` then `bus.publish()` inline — loses events whenever the
//! process crashes between the database write and the publish. The outbox fixes this: every
//! appended event is first **staged** into an `outbox` table, and a [`OutboxRelay`] drains
//! pending rows, publishes each to the [`EventBus`], and only then marks it delivered. A crash
//! simply leaves the row undelivered, so the next relay pass redelivers it (at-least-once).
//!
//! This pairs with the durable [`crate::PostgresEventStore`] mirror: the `events` table is the
//! source of truth, and the outbox is the delivery guarantee. For strict exactly-once-ish
//! semantics, stage the outbox row inside the *same* transaction as the `events` insert (see
//! [`crate::PostgresEventStore::with_outbox`], which awaits the stage alongside the mirror).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tpt_erp_bus::{BusError, EventBus};

/// Errors raised by the outbox store / relay.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("outbox storage error: {0}")]
    Storage(String),
    #[error("outbox relay publish error: {0}")]
    Publish(#[from] BusError),
}

/// A single staged, not-yet-delivered (or delivered) message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxRecord {
    /// Monotonic row id (delivery order).
    pub id: i64,
    /// Routing subject, e.g. `ledger.TransactionPosted`.
    pub subject: String,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
    /// Whether the relay has published and acknowledged this row.
    pub delivered: bool,
    /// When the row was staged.
    pub created_at: DateTime<Utc>,
}

/// Durable staging surface for the outbox. Implemented by [`InMemoryOutbox`] (tests) and
/// [`PostgresOutbox`] (production).
#[async_trait]
pub trait Outbox: Send + Sync {
    /// Stage a message, returning its assigned id.
    async fn stage(&self, subject: &str, payload: &[u8]) -> Result<i64, OutboxError>;
    /// Fetch up to `limit` not-yet-delivered rows, ordered by id.
    async fn fetch_pending(&self, limit: usize) -> Result<Vec<OutboxRecord>, OutboxError>;
    /// Mark a row delivered (call only after a successful publish).
    async fn mark_delivered(&self, id: i64) -> Result<(), OutboxError>;
}

/// In-process reference outbox (for tests and single-node runs).
#[derive(Default)]
pub struct InMemoryOutbox {
    inner: std::sync::Mutex<InMemoryInner>,
}

#[derive(Default)]
struct InMemoryInner {
    rows: Vec<OutboxRecord>,
    next_id: i64,
}

impl InMemoryOutbox {
    /// Create an empty in-memory outbox.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Outbox for InMemoryOutbox {
    async fn stage(&self, subject: &str, payload: &[u8]) -> Result<i64, OutboxError> {
        let mut g = self.inner.lock().unwrap();
        g.next_id += 1;
        let id = g.next_id;
        g.rows.push(OutboxRecord {
            id,
            subject: subject.to_string(),
            payload: payload.to_vec(),
            delivered: false,
            created_at: Utc::now(),
        });
        Ok(id)
    }

    async fn fetch_pending(&self, limit: usize) -> Result<Vec<OutboxRecord>, OutboxError> {
        let g = self.inner.lock().unwrap();
        Ok(g.rows
            .iter()
            .filter(|r| !r.delivered)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn mark_delivered(&self, id: i64) -> Result<(), OutboxError> {
        let mut g = self.inner.lock().unwrap();
        if let Some(r) = g.rows.iter_mut().find(|r| r.id == id) {
            r.delivered = true;
        }
        Ok(())
    }
}

/// Drains an [`Outbox`] onto an [`EventBus`] with durable at-least-once delivery.
#[derive(Clone)]
pub struct OutboxRelay<B: EventBus> {
    outbox: Arc<dyn Outbox>,
    bus: Arc<B>,
}

impl<B: EventBus> OutboxRelay<B> {
    /// Build a relay that publishes pending outbox rows to `bus`.
    pub fn new(outbox: Arc<dyn Outbox>, bus: Arc<B>) -> Self {
        Self { outbox, bus }
    }

    /// Publish every currently-pending row to the bus, marking each delivered only after a
    /// successful publish. Returns the number of rows delivered this pass. A publish failure
    /// leaves the row undelivered so it is retried on the next pass (at-least-once).
    pub async fn run_once(&self, limit: usize) -> Result<usize, OutboxError> {
        let pending = self.outbox.fetch_pending(limit).await?;
        let n = pending.len();
        for rec in pending {
            // `?` converts `BusError` into `OutboxError::Publish`; the row stays undelivered.
            self.bus.publish(&rec.subject, &rec.payload).await?;
            self.outbox.mark_delivered(rec.id).await?;
        }
        Ok(n)
    }

    /// Run the relay forever, sleeping `period` between passes. Any tick error is logged and
    /// retried next pass rather than terminating the relay.
    pub async fn run_forever(&self, period: Duration, limit: usize) {
        loop {
            if let Err(e) = self.run_once(limit).await {
                tracing::warn!(error = %e, "outbox relay tick failed");
            }
            tokio::time::sleep(period).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tpt_erp_bus::EventBus;
    use tpt_erp_bus::memory::InMemoryBus;

    #[tokio::test]
    async fn relay_publishes_pending_then_marks_delivered() {
        let bus = Arc::new(InMemoryBus::new());
        let outbox = Arc::new(InMemoryOutbox::new());

        outbox.stage("ledger.A", b"a1").await.unwrap();
        outbox.stage("ledger.B", b"b1").await.unwrap();

        let mut sink = bus.subscribe("ledger.>").await.unwrap();
        let delivered = OutboxRelay::new(outbox.clone(), bus.clone())
            .run_once(10)
            .await
            .unwrap();
        assert_eq!(delivered, 2);

        // Both messages reached the bus.
        let m1 = sink.next().await.unwrap();
        assert_eq!(m1.payload, b"a1");
        let m2 = sink.next().await.unwrap();
        assert_eq!(m2.payload, b"b1");

        // A second pass finds nothing left to deliver.
        let again = OutboxRelay::new(outbox.clone(), bus.clone())
            .run_once(10)
            .await
            .unwrap();
        assert_eq!(again, 0);
    }

    #[tokio::test]
    async fn publish_failure_leaves_row_pending() {
        // A bus whose publish always errors; the row must remain undelivered.
        let failing = Arc::new(FailingBus);
        let outbox = Arc::new(InMemoryOutbox::new());
        outbox.stage("ledger.A", b"a1").await.unwrap();

        let err = OutboxRelay::new(outbox.clone(), failing).run_once(10).await;
        assert!(err.is_err());

        // Still pending (redeliverable).
        assert_eq!(outbox.fetch_pending(10).await.unwrap().len(), 1);
    }

    struct FailingBus;
    #[async_trait]
    impl EventBus for FailingBus {
        async fn publish(&self, _subject: &str, _payload: &[u8]) -> Result<(), BusError> {
            Err(BusError::Backend("boom".to_string()))
        }
        async fn subscribe(&self, _subject: &str) -> Result<tpt_erp_bus::MessageStream, BusError> {
            unimplemented!()
        }
    }
}

#[cfg(feature = "postgres")]
mod postgres {
    //! Postgres-backed outbox. The `ledger_outbox` table is the durable staging area; the
    //! [`super::OutboxRelay`] drains it onto the bus.

    use super::*;
    use sqlx::postgres::PgPool;

    /// Postgres implementation of [`Outbox`].
    pub struct PostgresOutbox {
        pool: PgPool,
    }

    impl PostgresOutbox {
        /// Build from an existing pool.
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }

        /// Create the `ledger_outbox` table if it does not exist. Idempotent.
        pub async fn create_table(&self) -> Result<(), OutboxError> {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS ledger_outbox (\
                    id BIGSERIAL PRIMARY KEY, \
                    subject TEXT NOT NULL, \
                    payload BYTEA NOT NULL, \
                    delivered BOOLEAN NOT NULL DEFAULT FALSE, \
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            )
            .execute(&self.pool)
            .await
            .map_err(|e| OutboxError::Storage(e.to_string()))?;
            Ok(())
        }
    }

    #[async_trait]
    impl Outbox for PostgresOutbox {
        async fn stage(&self, subject: &str, payload: &[u8]) -> Result<i64, OutboxError> {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO ledger_outbox (subject, payload) VALUES ($1, $2) RETURNING id",
            )
            .bind(subject)
            .bind(payload)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| OutboxError::Storage(e.to_string()))?;
            Ok(id)
        }

        async fn fetch_pending(&self, limit: usize) -> Result<Vec<OutboxRecord>, OutboxError> {
            let rows = sqlx::query_as::<_, (i64, String, Vec<u8>, bool, DateTime<Utc>)>(
                "SELECT id, subject, payload, delivered, created_at FROM ledger_outbox \
                 WHERE NOT delivered ORDER BY id ASC LIMIT $1",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| OutboxError::Storage(e.to_string()))?;
            Ok(rows
                .into_iter()
                .map(
                    |(id, subject, payload, delivered, created_at)| OutboxRecord {
                        id,
                        subject,
                        payload,
                        delivered,
                        created_at,
                    },
                )
                .collect())
        }

        async fn mark_delivered(&self, id: i64) -> Result<(), OutboxError> {
            sqlx::query("UPDATE ledger_outbox SET delivered = TRUE WHERE id = $1")
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| OutboxError::Storage(e.to_string()))?;
            Ok(())
        }
    }
}

#[cfg(feature = "postgres")]
pub use postgres::PostgresOutbox;
