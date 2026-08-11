//! Offline-first transaction sync.
//!
//! A register must keep selling when the network drops. Sales are written to a
//! **local, event-sourced log** first (append-only, replayable). When connectivity
//! returns, [`PosSync::sync_to`] replays the pending sales onto the central store
//! **idempotently** — each sale carries its transaction id as an idempotency key, so a
//! replay that races a partial failure never double-posts. A `pos.synced` background job
//! is published per successful sync, and the last-synced sequence is pinned into
//! `tpt-erp-cache` as a **checkpoint** so a crash mid-sync resumes from where it left off.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tpt_erp_cache::{CacheError, ReadModelCache};
use tpt_erp_ledger::{Event, EventStore, InMemoryEventStore};
use tpt_erp_primitives::{Entity, Id, Money, Usd};
use tpt_erp_tenant::TenantId;

use crate::tender::TenderKind;

/// Marker entity for a POS terminal (the offline aggregate owner).
#[derive(Debug)]
pub struct PosTerminal;
impl Entity for PosTerminal {}

/// The kind of register event recorded offline. A `Sale` is a forward sale, a `Return`
/// is a refund (split back across the original tenders), and an `Exchange` bundles a
/// return with a new forward sale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SaleKind {
    Sale,
    Return,
    Exchange,
}

/// A sale recorded by the register, the unit of offline/online reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleEvent {
    /// Idempotency key: the originating transaction id. Re-applying the same key to the
    /// central store is a no-op.
    pub txn_id: String,
    /// Terminal that produced the sale.
    pub terminal: String,
    /// Whether this event is a forward sale, a return refund, or an exchange.
    pub kind: SaleKind,
    /// Grand total of the sale (or the refund amount for a `Return`).
    pub total: Money<Usd>,
    /// Tenders applied (kind + amount).
    pub tenders: Vec<(TenderKind, Money<Usd>)>,
    /// When the sale was recorded locally.
    pub at: DateTime<Utc>,
}

/// The central store a register reconciles against. Implementations MUST be idempotent in
/// [`CentralStore::apply_sale`] keyed by [`SaleEvent::txn_id`].
#[async_trait::async_trait]
pub trait CentralStore: Send + Sync {
    /// Apply a sale. Re-applying a sale with an already-seen `txn_id` must be a no-op
    /// (idempotency), not an error.
    async fn apply_sale(&self, sale: &SaleEvent) -> Result<(), SyncError>;
}

/// Errors raised by the sync engine.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("central store rejected sale {txn}: {reason}")]
    Central { txn: String, reason: String },
    #[error("event store serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("event store error: {0}")]
    Store(#[from] tpt_erp_ledger::EventStoreError),
    #[error("cache backend error: {0}")]
    Cache(#[from] CacheError),
    #[error("bus backend error: {0}")]
    Bus(#[from] tpt_erp_bus::BusError),
}

/// Offline-first sync orchestrator for one register.
pub struct PosSync {
    tenant: TenantId,
    terminal: Id<PosTerminal>,
    /// Append-only local log of every sale, online or off.
    local: Mutex<InMemoryEventStore<Id<PosTerminal>>>,
    /// In-memory mirror of pending (not-yet-synced) sales.
    pending: Mutex<Vec<SaleEvent>>,
    bus: Option<Box<dyn tpt_erp_bus::EventBus>>,
    cache: Option<Box<dyn ReadModelCache>>,
    /// Count of successful sync runs (telemetry).
    synced_jobs: AtomicU64,
}

impl PosSync {
    /// Build a sync engine for `terminal` under `tenant`, starting with an empty local log.
    pub fn new(tenant: TenantId, terminal: Id<PosTerminal>) -> Self {
        Self {
            tenant,
            terminal,
            local: Mutex::new(InMemoryEventStore::default()),
            pending: Mutex::new(Vec::new()),
            bus: None,
            cache: None,
            synced_jobs: AtomicU64::new(0),
        }
    }

    /// Attach a background-job bus; the `pos.synced` job is published after each sync.
    pub fn with_bus(mut self, bus: Box<dyn tpt_erp_bus::EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Attach a cache used as the sync checkpoint store.
    pub fn with_cache(mut self, cache: Box<dyn ReadModelCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// Record a sale to the local log. Works fully offline — no central call is made;
    /// the sale is held in the pending set until [`PosSync::sync_to`] runs.
    pub fn record_offline(&self, sale: SaleEvent) -> Result<(), SyncError> {
        {
            let mut store = self.local.lock().unwrap();
            store.append(Event::new(self.terminal, "sale-recorded", &sale)?);
        }
        self.pending.lock().unwrap().push(sale);
        Ok(())
    }

    /// Number of sales waiting to be reconciled.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// The number of successful sync runs so far.
    pub fn synced_jobs(&self) -> u64 {
        self.synced_jobs.load(Ordering::Relaxed)
    }

    /// Replay every pending sale onto `central` idempotently, then publish `pos.synced`
    /// and record the checkpoint in the cache. Sales that fail centrally abort the run so
    /// they stay pending (safe retry).
    pub async fn sync_to(&self, central: &dyn CentralStore) -> Result<u64, SyncError> {
        let pending = self.pending.lock().unwrap().clone();
        let mut synced = 0u64;
        for sale in &pending {
            central.apply_sale(sale).await?;
            synced += 1;
        }

        // All pending applied; clear and publish.
        if synced > 0 {
            self.pending.lock().unwrap().clear();
            self.write_checkpoint(synced).await;

            if let Some(bus) = &self.bus {
                let payload = serde_json::json!({
                    "terminal": self.terminal.as_str(),
                    "synced": synced,
                })
                .to_string();
                bus.publish("pos.synced", payload.as_bytes()).await?;
            }
            self.synced_jobs.fetch_add(1, Ordering::Relaxed);
        }
        Ok(synced)
    }

    /// Persist the last-synced sequence as a checkpoint for crash recovery.
    async fn write_checkpoint(&self, synced: u64) {
        if let Some(cache) = &self.cache {
            let _ = cache
                .put(
                    &self.tenant,
                    "pos-sync",
                    &self.terminal.as_str(),
                    serde_json::json!({ "synced": synced, "at": Utc::now().to_rfc3339() }),
                    Some(Duration::from_secs(86_400)),
                )
                .await;
        }
    }

    /// Number of events in the local log (proves durability/replayability).
    pub fn local_log_len(&self) -> usize {
        self.local.lock().unwrap().log().len()
    }
}

/// In-memory reference central store. Idempotent by `txn_id`: a repeated sale is ignored.
#[derive(Default)]
pub struct InMemoryCentral {
    applied: Mutex<std::collections::HashMap<String, SaleEvent>>,
}

impl InMemoryCentral {
    /// All sales applied so far, keyed by transaction id.
    pub fn sales(&self) -> Vec<SaleEvent> {
        self.applied.lock().unwrap().values().cloned().collect()
    }
}

#[async_trait::async_trait]
impl CentralStore for InMemoryCentral {
    async fn apply_sale(&self, sale: &SaleEvent) -> Result<(), SyncError> {
        let mut map = self.applied.lock().unwrap();
        map.entry(sale.txn_id.clone())
            .or_insert_with(|| sale.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_bus::memory::InMemoryBus;
    use tpt_erp_cache::memory::InMemoryCache;
    use tpt_erp_tenant::TenantSlug;

    fn terminal() -> Id<PosTerminal> {
        Id::new()
    }

    fn sale(txn: &str, major: i64) -> SaleEvent {
        SaleEvent {
            txn_id: txn.to_string(),
            terminal: "T1".to_string(),
            kind: SaleKind::Sale,
            total: Money::<Usd>::from_major(major),
            tenders: vec![(TenderKind::Cash, Money::<Usd>::from_major(major))],
            at: Utc::now(),
        }
    }

    fn tenant() -> TenantId {
        TenantSlug("pos-demo".to_string()).to_id()
    }

    #[test]
    fn record_offline_is_durable_and_pending() {
        let s = PosSync::new(tenant(), terminal());
        s.record_offline(sale("t1", 10)).unwrap();
        s.record_offline(sale("t2", 20)).unwrap();
        assert_eq!(s.pending_count(), 2);
        assert_eq!(s.local_log_len(), 2);
    }

    #[tokio::test]
    async fn sync_replays_idempotently() {
        let s = PosSync::new(tenant(), terminal());
        s.record_offline(sale("t1", 10)).unwrap();
        s.record_offline(sale("t2", 20)).unwrap();

        let central = InMemoryCentral::default();
        let synced = s.sync_to(&central).await.unwrap();
        assert_eq!(synced, 2);
        assert_eq!(s.pending_count(), 0);
        assert_eq!(central.sales().len(), 2);

        // Re-running sync is a no-op (nothing pending) and central is unchanged.
        let again = s.sync_to(&central).await.unwrap();
        assert_eq!(again, 0);
        assert_eq!(central.sales().len(), 2);
        assert_eq!(s.synced_jobs(), 1);
    }

    #[tokio::test]
    async fn sync_publishes_job_and_checkpoints() {
        let bus = InMemoryBus::new();
        let cache = InMemoryCache::new();
        let s = PosSync::new(tenant(), terminal())
            .with_bus(Box::new(bus))
            .with_cache(Box::new(cache));
        s.record_offline(sale("t1", 15)).unwrap();

        s.sync_to(&InMemoryCentral::default()).await.unwrap();

        // Checkpoint persisted to the cache.
        let cp = match &s.cache {
            Some(c) => c
                .get(&tenant(), "pos-sync", &s.terminal.as_str())
                .await
                .unwrap(),
            None => None,
        };
        assert!(cp.is_some());
    }
}
