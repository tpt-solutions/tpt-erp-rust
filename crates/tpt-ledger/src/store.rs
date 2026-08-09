//! Append-only, in-memory event store with optimistic-concurrency control.
//!
//! This is the reference implementation of the ledger's event log. A production backend
//! would persist the same [`StoredEvent`] shape to Postgres; the per-aggregate `sequence`
//! is what makes appends idempotent and conflict-detecting.

use crate::event::{Event, EventStoreError, StoredEvent};
use chrono::Utc;
use std::collections::HashMap;
use std::hash::Hash;

/// An append-only event log keyed by aggregate.
#[derive(Debug, Clone, Default)]
pub struct EventStore<A> {
    log: Vec<StoredEvent<A>>,
    versions: HashMap<A, u64>,
}

impl<A> EventStore<A>
where
    A: Clone + Eq + Hash,
{
    /// Append without a version check, returning the stored event with its assigned
    /// `sequence` (current version + 1).
    pub fn append(&mut self, event: Event<A>) -> StoredEvent<A> {
        let current = self.versions.get(&event.aggregate_id).copied().unwrap_or(0);
        self.append_versioned(event, current + 1)
            .expect("sequential append cannot conflict")
    }

    /// Append, asserting the aggregate is currently at `expected` version. This is the
    /// optimistic-concurrency guard: a concurrent writer that advanced the version first
    /// causes a [`EventStoreError::Conflict`] here.
    pub fn append_versioned(
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

    /// Events for a single aggregate, in sequence order.
    pub fn stream(&self, aggregate_id: &A) -> Vec<&StoredEvent<A>> {
        self.log
            .iter()
            .filter(|e| &e.aggregate_id == aggregate_id)
            .collect()
    }

    /// The full event log, in append order.
    pub fn log(&self) -> &[StoredEvent<A>] {
        &self.log
    }

    /// Current version (last sequence) of an aggregate, or 0 if none yet.
    pub fn version(&self, aggregate_id: &A) -> u64 {
        self.versions.get(aggregate_id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_entry::{Account, AccountId};

    #[test]
    fn sequences_increase_per_aggregate() {
        let mut store: EventStore<AccountId> = EventStore::default();
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
        let mut store: EventStore<AccountId> = EventStore::default();
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
    fn _account_marker_compiles() {
        let _ = std::marker::PhantomData::<Account>;
    }
}
