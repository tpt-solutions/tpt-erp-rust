//! Append-only event store with optimistic-concurrency control.
//!
//! The [`EventStore`] trait defines the append-only log used by the ledger. This module
//! ships [`InMemoryEventStore`] as the reference implementation; a production backend
//! (e.g. Postgres) implements the same trait and persists the identical [`StoredEvent`]
//! shape, so the per-aggregate `sequence` makes appends idempotent and conflict-detecting
//! regardless of storage engine.

use crate::event::{Event, EventStoreError, StoredEvent};
use chrono::Utc;
use std::collections::HashMap;
use std::hash::Hash;

/// An append-only event log keyed by aggregate.
///
/// Implementations guarantee that each aggregate's events form a contiguous 1-based
/// sequence and that appends can be guarded by an expected version (optimistic
/// concurrency). A real storage backend implements this trait and persists the same
/// [`StoredEvent`] shape (see the in-memory [`InMemoryEventStore`]).
pub trait EventStore<A: Clone + Eq + Hash> {
    /// Append without a version check, returning the stored event with its assigned
    /// `sequence` (current version + 1).
    fn append(&mut self, event: Event<A>) -> StoredEvent<A>;

    /// Append, asserting the aggregate is currently at `expected` version. This is the
    /// optimistic-concurrency guard: a concurrent writer that advanced the version first
    /// causes a [`EventStoreError::Conflict`] here.
    fn append_versioned(
        &mut self,
        event: Event<A>,
        expected: u64,
    ) -> Result<StoredEvent<A>, EventStoreError>;

    /// Events for a single aggregate, in sequence order.
    fn stream(&self, aggregate_id: &A) -> Vec<&StoredEvent<A>>;

    /// The full event log, in append order.
    fn log(&self) -> &[StoredEvent<A>];

    /// Current version (last sequence) of an aggregate, or 0 if none yet.
    fn version(&self, aggregate_id: &A) -> u64;
}

/// The reference [`EventStore`] implementation: an in-memory append-only log.
///
/// Suitable for tests, demos, and single-node deployments. A Postgres-backed store
/// implements the same trait and can be swapped in without touching callers.
#[derive(Debug, Clone)]
pub struct InMemoryEventStore<A> {
    log: Vec<StoredEvent<A>>,
    versions: HashMap<A, u64>,
}

impl<A> Default for InMemoryEventStore<A> {
    fn default() -> Self {
        Self {
            log: Vec::new(),
            versions: HashMap::new(),
        }
    }
}

impl<A> InMemoryEventStore<A>
where
    A: Clone + Eq + Hash,
{
    /// Create an empty in-memory event store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<A> EventStore<A> for InMemoryEventStore<A>
where
    A: Clone + Eq + Hash,
{
    fn append(&mut self, event: Event<A>) -> StoredEvent<A> {
        let current = self.versions.get(&event.aggregate_id).copied().unwrap_or(0);
        self.append_versioned(event, current + 1)
            .expect("sequential append cannot conflict")
    }

    fn append_versioned(
        &mut self,
        event: Event<A>,
        expected: u64,
    ) -> Result<StoredEvent<A>, EventStoreError> {
        let current = self.versions.get(&event.aggregate_id).copied().unwrap_or(0);
        if expected != current + 1 {
            return Err(EventStoreError::Conflict { expected, current });
        }
        let sequence = current + 1;
        let stored = StoredEvent {
            aggregate_id: event.aggregate_id.clone(),
            sequence,
            event_type: event.event_type,
            payload: event.payload,
            occurred_at: Utc::now(),
        };
        self.versions.insert(event.aggregate_id, sequence);
        self.log.push(stored.clone());
        Ok(stored)
    }

    fn stream(&self, aggregate_id: &A) -> Vec<&StoredEvent<A>> {
        self.log
            .iter()
            .filter(|e| &e.aggregate_id == aggregate_id)
            .collect()
    }

    fn log(&self) -> &[StoredEvent<A>] {
        &self.log
    }

    fn version(&self, aggregate_id: &A) -> u64 {
        self.versions.get(aggregate_id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_entry::{Account, AccountId};

    #[test]
    fn sequences_increase_per_aggregate() {
        let mut store: InMemoryEventStore<AccountId> = InMemoryEventStore::default();
        let a1 = AccountId::new();
        let a2 = AccountId::new();

        let e1 = store.append(Event::new(a1, "Created", &"x").unwrap());
        let e2 = store.append(Event::new(a1, "Updated", &"y").unwrap());
        let e3 = store.append(Event::new(a2, "Created", &"z").unwrap());

        assert_eq!(e1.sequence, 1);
        assert_eq!(e2.sequence, 2);
        assert_eq!(e3.sequence, 1);
        assert_eq!(store.version(&a1), 2);
        assert_eq!(store.version(&a2), 1);
        assert_eq!(store.stream(&a1).len(), 2);
    }

    #[test]
    fn optimistic_concurrency_detects_conflict() {
        let mut store: InMemoryEventStore<AccountId> = InMemoryEventStore::default();
        let acc = AccountId::new();
        store.append(Event::new(acc, "Created", &"x").unwrap());

        // Replaying from a stale expected version must be rejected.
        let stale = store.append_versioned(Event::new(acc, "Updated", &"y").unwrap(), 1);
        assert!(matches!(stale, Err(EventStoreError::Conflict { .. })));

        // Correct next version succeeds.
        let ok = store.append_versioned(Event::new(acc, "Updated", &"y").unwrap(), 2);
        assert!(ok.is_ok());
    }

    #[test]
    fn in_memory_satisfies_trait() {
        fn assert_store<A: Clone + Eq + Hash>(_s: &impl EventStore<A>) {}
        let store: InMemoryEventStore<AccountId> = InMemoryEventStore::default();
        assert_store(&store);
    }

    #[test]
    fn _account_marker_compiles() {
        let _ = std::marker::PhantomData::<Account>;
    }
}
