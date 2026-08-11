//! # tpt-erp-ledger
//!
//! The financial and audit heart of TPT ERP.
//!
//! - [`event`] — the append-only event schema ([`StoredEvent`], [`Event`]).
//! - [`store`] — an in-memory append-only [`EventStore`] with optimistic concurrency.
//! - [`double_entry`] — the [`DoubleEntry`] trait enforcing that transactions balance.
//! - [`projection`] — the CQRS [`Projector`] trait, [`replay`], and an example
//!   [`BalanceProjection`] read model.
//!
//! A production deployment persists the same [`StoredEvent`] shape to Postgres and runs
//! the same projectors; only the storage backend changes.

pub mod double_entry;
pub mod event;
pub mod projection;
pub mod store;

pub use double_entry::{
    Account, AccountId, DoubleEntry, DoubleEntryError, EntrySide, LedgerEntry, LedgerEvent,
    LedgerTransaction, Transaction, TransactionId,
};
pub use event::{Event, EventStoreError, StoredEvent};
pub use projection::{BalanceProjection, ProjectionError, Projector, replay};
pub use store::{EventStore, InMemoryEventStore};
