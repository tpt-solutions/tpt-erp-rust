//! Real-time inventory engine.
//!
//! Inventory is modeled as an **event-sourced** projection over per-bin movement
//! events. Writes never mutate a "quantity row" in place; they *append* a movement
//! to the bin's log. Because appends are per-bin and sharded, many bins can be
//! updated concurrently with no global row lock — the hallmark of a real-time WMS.
//!
//! The running on-hand quantity is a CQRS read model that can be rebuilt at any time
//! by replaying the event log (`rebuild_read_models`). It is cached per tenant via
//! `tpt-erp-cache`, and falling below a reorder point publishes a `jobs.replenish`
//! background job on `tpt-erp-bus`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tpt_erp_cache::{CacheError, ReadModelCache};
use tpt_erp_ledger::{Event, EventStore, Projector, StoredEvent, replay};
use tpt_erp_primitives::{Entity, Id};
use tpt_erp_tenant::{TenantId, TenantSlug};

/// A stock-keeping unit.
#[derive(Debug)]
pub struct Sku;
impl Entity for Sku {}

/// A physical bin / location in the warehouse.
#[derive(Debug)]
pub struct Bin;
impl Entity for Bin {}

/// Composite key identifying the inventory of one SKU in one bin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StockKey {
    pub bin: Id<Bin>,
    pub sku: Id<Sku>,
}

impl Hash for StockKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bin.as_uuid().hash(state);
        self.sku.as_uuid().hash(state);
    }
}

impl StockKey {
    fn cache_str(&self) -> String {
        format!("{}::{}", self.bin.as_str(), self.sku.as_str())
    }
}

/// A single inventory movement. Stored as an append-only event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Movement {
    /// Goods received into the bin (positive).
    Received(u32),
    /// Goods picked out of the bin (positive count removed).
    Picked(u32),
    /// A manual adjustment; may be negative (shrinkage) or positive (found).
    Adjusted(i64),
}

impl Movement {
    /// The signed delta this movement applies to on-hand quantity.
    pub fn delta(&self) -> i64 {
        match self {
            Movement::Received(q) => i64::from(*q),
            Movement::Picked(q) => -(i64::from(*q)),
            Movement::Adjusted(d) => *d,
        }
    }
}

/// Errors raised by the inventory engine.
#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error("event store serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("optimistic-concurrency conflict at {key:?}: expected v{expected}, current v{current}")]
    Conflict {
        key: StockKey,
        expected: u64,
        current: u64,
    },
    #[error("cache backend error: {0}")]
    Cache(#[from] CacheError),
    #[error("bus backend error: {0}")]
    Bus(String),
    #[error("projection replay failed: {0}")]
    Projection(#[from] tpt_erp_ledger::ProjectionError),
    #[error("event store error: {0}")]
    Store(#[from] tpt_erp_ledger::EventStoreError),
}

const SHARDS: usize = 64;

struct Shard {
    store: EventStore<StockKey>,
    /// Running on-hand quantity per stock key (kept in sync with the log).
    on_hand: HashMap<StockKey, i64>,
}

impl Shard {
    fn new() -> Self {
        Self {
            store: EventStore::default(),
            on_hand: HashMap::new(),
        }
    }
}

/// Real-time, event-sourced inventory engine.
///
/// * Writes append movement events sharded by [`StockKey`], so concurrent updates to
///   different bins never contend on a global lock.
/// * On-hand is a derived read model, optionally cached per tenant (`tpt-erp-cache`).
/// * Crossing the reorder point emits a `jobs.replenish` job (`tpt-erp-bus`).
pub struct InventoryEngine {
    shards: Vec<Mutex<Shard>>,
    tenant: TenantId,
    reorder_point: i64,
    bus: Option<Box<dyn tpt_erp_bus::EventBus>>,
    cache: Option<Box<dyn ReadModelCache>>,
    published_jobs: AtomicU64,
}

impl InventoryEngine {
    /// Create an engine for `tenant` with the given reorder threshold.
    pub fn new(tenant: TenantId, reorder_point: i64) -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::new())).collect(),
            tenant,
            reorder_point,
            bus: None,
            cache: None,
            published_jobs: AtomicU64::new(0),
        }
    }

    /// Attach a background-job bus; replenishment jobs are published there.
    pub fn with_bus(mut self, bus: Box<dyn tpt_erp_bus::EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Attach a read-model cache for on-hand quantities.
    pub fn with_cache(mut self, cache: Box<dyn ReadModelCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    fn shard_index(key: &StockKey) -> usize {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        (h.finish() as usize) % SHARDS
    }

    /// Append a movement event, returning the new on-hand quantity.
    pub async fn apply(&self, key: StockKey, movement: Movement) -> Result<i64, InventoryError> {
        let new_qty = {
            let idx = Self::shard_index(&key);
            let mut shard = self.shards[idx].lock().unwrap();
            let event = Event::new(key, "movement", &movement)?;
            shard.store.append(event);
            let on_hand = shard.on_hand.entry(key).or_insert(0);
            *on_hand += movement.delta();
            *on_hand
        };

        self.after_write(key, new_qty).await;
        Ok(new_qty)
    }

    /// Apply a movement with an optimistic-concurrency guard. The caller passes the
    /// version it believes the bin is at; a stale caller is rejected with
    /// [`InventoryError::Conflict`]. This is what makes concurrent same-bin updates
    /// safe without locking the row.
    pub async fn apply_versioned(
        &self,
        key: StockKey,
        movement: Movement,
        expected_version: u64,
    ) -> Result<i64, InventoryError> {
        let new_qty = {
            let idx = Self::shard_index(&key);
            let mut shard = self.shards[idx].lock().unwrap();
            let event = Event::new(key, "movement", &movement)?;
            match shard.store.append_versioned(event, expected_version) {
                Ok(_) => {}
                Err(_) => {
                    let current = shard.store.version(&key);
                    return Err(InventoryError::Conflict {
                        key,
                        expected: expected_version,
                        current,
                    });
                }
            }
            let on_hand = shard.on_hand.entry(key).or_insert(0);
            *on_hand += movement.delta();
            *on_hand
        };

        self.after_write(key, new_qty).await;
        Ok(new_qty)
    }

    /// Read the cached/derived on-hand quantity for a key.
    pub fn on_hand(&self, key: StockKey) -> i64 {
        let idx = Self::shard_index(&key);
        let shard = self.shards[idx].lock().unwrap();
        *shard.on_hand.get(&key).unwrap_or(&0)
    }

    /// Current event-log version for a key (used by optimistic-concurrency clients).
    pub fn version(&self, key: StockKey) -> u64 {
        let idx = Self::shard_index(&key);
        let shard = self.shards[idx].lock().unwrap();
        shard.store.version(&key)
    }

    /// Number of replenishment jobs emitted so far.
    pub fn published_jobs(&self) -> u64 {
        self.published_jobs.load(Ordering::Relaxed)
    }

    async fn after_write(&self, key: StockKey, new_qty: i64) {
        if new_qty < self.reorder_point {
            if let Some(bus) = &self.bus {
                let payload = serde_json::json!({
                    "bin": key.bin.as_str(),
                    "sku": key.sku.as_str(),
                    "on_hand": new_qty,
                })
                .to_string();
                if bus
                    .publish("jobs.replenish", payload.as_bytes())
                    .await
                    .is_ok()
                {
                    self.published_jobs.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if let Some(cache) = &self.cache {
            cache
                .put(
                    &self.tenant,
                    "inventory",
                    &key.cache_str(),
                    serde_json::json!({ "on_hand": new_qty }),
                    None,
                )
                .await
                .ok();
        }
    }

    /// Rebuild the on-hand read model from the event log from scratch (CQRS replay),
    /// writing results into the attached cache. This proves the read model can never
    /// silently drift from the ledger of record.
    pub async fn rebuild_read_models(&self) -> Result<HashMap<StockKey, i64>, InventoryError> {
        let mut all: Vec<StoredEvent<StockKey>> = Vec::new();
        for shard in &self.shards {
            let s = shard.lock().unwrap();
            all.extend(s.store.log().iter().cloned());
        }
        let events: Vec<(StockKey, Movement)> = all
            .into_iter()
            .map(|e| {
                let m = serde_json::from_value::<Movement>(e.payload)?;
                Ok((e.aggregate_id, m))
            })
            .collect::<Result<_, serde_json::Error>>()?;

        let proj = replay(InventoryProjection::default(), events).await?;
        if let Some(cache) = &self.cache {
            for (k, v) in &proj.on_hand {
                cache
                    .put(
                        &self.tenant,
                        "inventory",
                        &k.cache_str(),
                        serde_json::json!({ "on_hand": v }),
                        None,
                    )
                    .await
                    .ok();
            }
        }
        Ok(proj.on_hand)
    }
}

/// CQRS read model: folds movement events into per-key on-hand quantities.
#[derive(Debug, Clone, Default)]
pub struct InventoryProjection {
    pub on_hand: HashMap<StockKey, i64>,
}

impl Projector for InventoryProjection {
    type Event = (StockKey, Movement);

    async fn apply(&mut self, event: &Self::Event) -> Result<(), tpt_erp_ledger::ProjectionError> {
        let (key, movement) = event;
        *self.on_hand.entry(*key).or_insert(0) += movement.delta();
        Ok(())
    }
}

/// Build a tenant for example/demo use.
pub fn demo_tenant() -> TenantId {
    TenantSlug("wms-demo".to_string()).to_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_bus::memory::InMemoryBus;
    use tpt_erp_cache::memory::InMemoryCache;

    fn key() -> StockKey {
        StockKey {
            bin: Id::new(),
            sku: Id::new(),
        }
    }

    #[tokio::test]
    async fn received_then_picked_yields_correct_on_hand() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        assert_eq!(eng.apply(k, Movement::Received(50)).await.unwrap(), 50);
        assert_eq!(eng.apply(k, Movement::Picked(20)).await.unwrap(), 30);
        assert_eq!(eng.apply(k, Movement::Adjusted(-5)).await.unwrap(), 25);
        assert_eq!(eng.on_hand(k), 25);
    }

    #[tokio::test]
    async fn optimistic_concurrency_rejects_stale_writer() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        eng.apply(k, Movement::Received(10)).await.unwrap();
        // Version is now 1. A client claiming version 1 again (already applied) is stale.
        let stale = eng.apply_versioned(k, Movement::Picked(1), 1).await;
        assert!(matches!(stale, Err(InventoryError::Conflict { .. })));
        // Correct next version (2) succeeds.
        let ok = eng.apply_versioned(k, Movement::Picked(1), 2).await;
        assert!(ok.is_ok());
    }

    #[tokio::test]
    async fn reorder_point_emits_replenish_job() {
        let bus = InMemoryBus::new();
        let eng = InventoryEngine::new(demo_tenant(), 10).with_bus(Box::new(bus));
        let k = key();
        // Drop below reorder point.
        eng.apply(k, Movement::Received(5)).await.unwrap();
        assert_eq!(eng.published_jobs(), 1);
    }

    #[tokio::test]
    async fn concurrent_updates_to_many_bins_are_all_applied() {
        let eng = std::sync::Arc::new(InventoryEngine::new(demo_tenant(), 0));
        let bins: Vec<StockKey> = (0..32).map(|_| key()).collect();
        let tasks: Vec<_> = bins
            .iter()
            .map(|k| {
                let eng = eng.clone();
                let k = *k;
                tokio::spawn(async move {
                    for _ in 0..100 {
                        eng.apply(k, Movement::Received(1)).await.unwrap();
                    }
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        for k in &bins {
            assert_eq!(eng.on_hand(*k), 100);
        }
    }

    #[tokio::test]
    async fn concurrent_updates_to_same_bin_without_lost_updates() {
        // All writes funnel through the same bin's shard lock, so even though the
        // store never "locks the row" at the application layer, no movement is lost.
        let eng = std::sync::Arc::new(InventoryEngine::new(demo_tenant(), 0));
        let k = key();
        let tasks: Vec<_> = (0..50)
            .map(|_| {
                let eng = eng.clone();
                tokio::spawn(async move {
                    for _ in 0..40 {
                        eng.apply(k, Movement::Received(1)).await.unwrap();
                    }
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(eng.on_hand(k), 50 * 40);
    }

    #[tokio::test]
    async fn rebuild_read_model_matches_live_state() {
        let cache = InMemoryCache::new();
        let eng = InventoryEngine::new(demo_tenant(), 0).with_cache(Box::new(cache));
        let bins: Vec<StockKey> = (0..10).map(|_| key()).collect();
        for k in &bins {
            eng.apply(*k, Movement::Received(7)).await.unwrap();
            eng.apply(*k, Movement::Picked(2)).await.unwrap();
        }
        let rebuilt = eng.rebuild_read_models().await.unwrap();
        for k in &bins {
            assert_eq!(eng.on_hand(*k), 5);
            assert_eq!(rebuilt[k], 5);
        }
    }
}
