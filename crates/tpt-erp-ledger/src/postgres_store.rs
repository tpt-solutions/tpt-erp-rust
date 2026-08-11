//! Postgres-backed [`EventStore`] implementation.
//!
//! The [`EventStore`] trait hands out borrowed `StoredEvent`s (`stream`/`log`), so a
//! pure-DB backend would have nowhere to borrow from. [`PostgresEventStore`] therefore
//! keeps an in-memory mirror (authoritative for conflict checks and the borrow-based
//! read API) that is durably mirrored to Postgres on every append, and hydrated from the
//! `events` table on startup via [`PostgresEventStore::load`]. Swap it in for
//! [`InMemoryEventStore`] without touching any caller that only depends on the `EventStore`
//! trait.

use std::hash::Hash;
use std::marker::PhantomData;

use chrono::Utc;
use sqlx::postgres::PgPool;

use crate::event::{Event, EventStoreError, StoredEvent};
use crate::store::{EventStore, InMemoryEventStore};

impl From<sqlx::Error> for EventStoreError {
    fn from(e: sqlx::Error) -> Self {
        EventStoreError::Backend(e.to_string())
    }
}

/// An [`EventStore`] mirrored to Postgres.
pub struct PostgresEventStore<A> {
    pool: PgPool,
    cache: InMemoryEventStore<A>,
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
        Self {
            pool,
            cache: InMemoryEventStore::default(),
            _marker: PhantomData,
        }
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
        let rows = sqlx::query_as::<_, (String, i64, String, serde_json::Value, chrono::DateTime<Utc>)>(
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

    /// Best-effort durable mirror of an appended event (fire-and-forget on the runtime).
    fn mirror(&self, stored: &StoredEvent<A>) {
        let pool = self.pool.clone();
        let aggregate_id = stored.aggregate_id.to_string();
        let sequence = stored.sequence as i64;
        let event_type = stored.event_type.clone();
        let payload = stored.payload.clone();
        let occurred_at = stored.occurred_at;
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO events (aggregate_id, sequence, event_type, payload, occurred_at) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(aggregate_id)
            .bind(sequence)
            .bind(event_type)
            .bind(payload)
            .bind(occurred_at)
            .execute(&pool)
            .await;
        });
    }
}

impl<A> EventStore<A> for PostgresEventStore<A>
where
    A: Clone + Eq + Hash + std::fmt::Display + std::str::FromStr + Send + Sync + 'static,
    <A as std::str::FromStr>::Err: std::fmt::Display,
{
    fn append(&mut self, event: Event<A>) -> StoredEvent<A> {
        let stored = self.cache.append(event);
        self.mirror(&stored);
        stored
    }

    fn append_versioned(
        &mut self,
        event: Event<A>,
        expected: u64,
    ) -> Result<StoredEvent<A>, EventStoreError> {
        let stored = self.cache.append_versioned(event, expected)?;
        self.mirror(&stored);
        Ok(stored)
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
