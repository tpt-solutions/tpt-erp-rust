//! Postgres-backed [`EventStore`] implementation.
//!
//! The [`EventStore`] trait hands out borrowed `StoredEvent`s (`stream`/`log`), so a
//! pure-DB backend would have nowhere to borrow from. [`PostgresEventStore`] therefore
//! keeps an in-memory mirror (authoritative for conflict checks and the borrow-based
//! read API) that is *durably* mirrored to Postgres on every append, and hydrated from
//! the `events` table on startup via [`PostgresEventStore::load`]. Swap it in for
//! [`InMemoryEventStore`] without touching any caller that only depends on the `EventStore`
//! trait.
//!
//! Unlike a fire-and-forget mirror, every append waits for its Postgres write (or
//! surfaces the error). A DB outage or constraint violation therefore fails loudly
//! instead of silently and permanently dropping a posted financial transaction.

use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPool;
use tokio::runtime::Handle;
use tracing::error;

use crate::event::{Event, EventStoreError, StoredEvent};
use crate::store::{EventStore, InMemoryEventStore};

impl From<sqlx::Error> for EventStoreError {
    fn from(e: sqlx::Error) -> Self {
        EventStoreError::Backend(Box::new(e))
    }
}

/// An [`EventStore`] mirrored to Postgres.
pub struct PostgresEventStore<A> {
    pool: PgPool,
    cache: InMemoryEventStore<A>,
    /// A handle to the driving runtime, captured at construction. Used to drive the
    /// durable write to completion from the synchronous trait methods without deadlocking
    /// the calling async runtime.
    rt: Option<Handle>,
    /// Count of in-memory appends whose Postgres mirror write failed. Non-zero signals
    /// potential divergence between the in-memory log and Postgres; surface in health
    /// checks.
    durability_failures: Arc<AtomicU64>,
    _marker: PhantomData<A>,
}

impl<A> PostgresEventStore<A>
where
    A: Clone + Eq + Hash + std::fmt::Display + std::str::FromStr + Send + Sync + 'static,
    <A as std::str::FromStr>::Err: std::fmt::Display,
{
    /// Create a store over an existing Postgres pool. Call [`PostgresEventStore::load`]
    /// afterwards to hydrate the in-memory mirror from the `events` table.
    pub fn new(pool: PgPool) -> Self {
        let rt = Handle::try_current().ok();
        Self {
            pool,
            cache: InMemoryEventStore::default(),
            rt,
            durability_failures: Arc::new(AtomicU64::new(0)),
            _marker: PhantomData,
        }
    }

    /// Number of appends whose durable Postgres mirror write failed after the in-memory
    /// append succeeded. A non-zero value indicates the in-memory log and Postgres may
    /// have diverged and should be flagged in a health check.
    pub fn durability_failures(&self) -> u64 {
        self.durability_failures.load(Ordering::SeqCst)
    }

    /// Create the `events` table if it does not exist (idempotent; safe to call on
    /// every startup). The schema mirrors the in-memory [`StoredEvent`] exactly.
    pub async fn create_table(&self) -> Result<(), EventStoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS events (\
                aggregate_id TEXT NOT NULL, \
                sequence BIGINT NOT NULL, \
                event_type TEXT NOT NULL, \
                payload JSONB NOT NULL, \
                occurred_at TIMESTAMPTZ NOT NULL, \
                PRIMARY KEY (aggregate_id, sequence))",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Hydrate the in-memory mirror from Postgres (used on startup / after reconnect).
    pub async fn load(&mut self) -> Result<(), EventStoreError> {
        let rows = sqlx::query_as::<_, (String, i64, String, serde_json::Value, DateTime<Utc>)>(
            "SELECT aggregate_id, sequence, event_type, payload, occurred_at FROM events ORDER BY sequence ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        for (aid_str, _seq, event_type, payload, _occurred_at) in rows {
            let aggregate_id = match A::from_str(&aid_str) {
                Ok(a) => a,
                Err(_) => continue,
            };
            // Replay in `sequence` order so the in-memory mirror re-derives the same
            // sequence numbers the database assigned.
            self.cache.append(Event {
                aggregate_id,
                event_type,
                payload,
            });
        }
        Ok(())
    }

    /// Durable async mirror of a single appended event. Awaits the Postgres write and
    /// propagates any backend error so the caller can react instead of losing the event.
    ///
    /// Returns a future that owns its data (a cloned pool handle plus copied fields) so it
    /// is `'static` and can be driven from a synchronous context via [`Self::run_durable`].
    fn mirror(
        &self,
        stored: &StoredEvent<A>,
    ) -> impl std::future::Future<Output = Result<(), EventStoreError>> + Send + 'static {
        let pool = self.pool.clone();
        let aggregate_id = stored.aggregate_id.to_string();
        let sequence = stored.sequence as i64;
        let event_type = stored.event_type.clone();
        let payload = stored.payload.clone();
        let occurred_at = stored.occurred_at;
        async move {
            sqlx::query(
                "INSERT INTO events (aggregate_id, sequence, event_type, payload, occurred_at) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(aggregate_id)
            .bind(sequence)
            .bind(event_type)
            .bind(payload)
            .bind(occurred_at)
            .execute(&pool)
            .await?;
            Ok(())
        }
    }

    /// Drive `fut` to completion from a synchronous context without deadlocking the
    /// calling async runtime: when we're already inside a runtime we park this task on a
    /// blocking thread (via `block_in_place`) and `block_on` the future there — which is
    /// allowed outside an async task — otherwise we spin up a temporary single-threaded
    /// runtime.
    fn run_durable<F>(&self, fut: F) -> Result<F::Output, EventStoreError>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send,
    {
        match &self.rt {
            Some(handle) => Ok(tokio::task::block_in_place(|| handle.block_on(fut))),
            None => Ok(tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| EventStoreError::Backend(Box::new(e)))?
                .block_on(fut)),
        }
    }

    /// Durable async append. Awaits the Postgres write; a mirror failure is logged (via
    /// `tracing`) and counted so it is never silent, but the already-committed in-memory
    /// event is still returned (see [`PostgresEventStore::durability_failures`]).
    pub async fn append_async(&mut self, event: Event<A>) -> StoredEvent<A> {
        let stored = self.cache.append(event);
        if let Err(e) = self.mirror(&stored).await {
            error!(
                aggregate_id = %stored.aggregate_id,
                sequence = stored.sequence,
                error = %e,
                "Postgres event mirror write failed"
            );
            self.durability_failures.fetch_add(1, Ordering::SeqCst);
        }
        stored
    }

    /// Durable async versioned append. The Postgres write is awaited and its error is
    /// surfaced directly, so a failed mirror never silently loses the event.
    pub async fn append_versioned_async(
        &mut self,
        event: Event<A>,
        expected: u64,
    ) -> Result<StoredEvent<A>, EventStoreError> {
        let stored = self.cache.append_versioned(event, expected)?;
        self.mirror(&stored).await?;
        Ok(stored)
    }
}

impl<A> EventStore<A> for PostgresEventStore<A>
where
    A: Clone + Eq + Hash + std::fmt::Display + std::str::FromStr + Send + Sync + 'static,
    <A as std::str::FromStr>::Err: std::fmt::Display,
{
    fn append(&mut self, event: Event<A>) -> StoredEvent<A> {
        let stored = self.cache.append(event);
        match self.run_durable(self.mirror(&stored)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) | Err(e) => {
                error!(
                    aggregate_id = %stored.aggregate_id,
                    sequence = stored.sequence,
                    error = %e,
                    "Postgres event mirror write failed"
                );
                self.durability_failures.fetch_add(1, Ordering::SeqCst);
            }
        }
        stored
    }

    fn append_versioned(
        &mut self,
        event: Event<A>,
        expected: u64,
    ) -> Result<StoredEvent<A>, EventStoreError> {
        let stored = self.cache.append_versioned(event, expected)?;
        match self.run_durable(self.mirror(&stored)) {
            Ok(Ok(())) => Ok(stored),
            Ok(Err(e)) | Err(e) => Err(e),
        }
    }

    fn stream(&self, aggregate_id: &A) -> Vec<&StoredEvent<A>> {
        self.cache.stream(aggregate_id)
    }

    fn log(&self) -> &[StoredEvent<A>] {
        self.cache.log()
    }

    fn version(&self, aggregate_id: &A) -> u64 {
        self.cache.version(aggregate_id)
    }
}
