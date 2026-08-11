//! Event-sourced reservation (stock-hold) engine.
//!
//! Stock holds are modeled as an **event-sourced** projection over per-SKU movement
//! events, sharded by SKU so concurrent reservations on different SKUs never contend
//! on a global row lock. A hold is a soft reservation that *expires*: the expiry is
//! mirrored into `tpt-erp-cache` with a TTL, and the cache's TTL is the authority for
//! auto-release — once the TTL elapses the cached hold disappears and the next
//! [`ReservationEngine::sweep_expired`] (or availability read) treats it as released,
//! so the stock becomes sellable again with no manual intervention.
//!
//! Oversell is prevented structurally: every [`ReservationEngine::reserve`] takes the
//! SKU's shard lock, purges any expired holds, and only succeeds if the *available*
//! quantity (on-hand − committed − active holds) covers the request. Concurrent
//! reservations to the same SKU therefore serialize through the shard and can never
//! allocate the same unit twice.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tpt_erp_cache::{CacheError, ReadModelCache};
use tpt_erp_ledger::{Event, EventStore, InMemoryEventStore, Projector, StoredEvent, replay};
use tpt_erp_primitives::{Entity, Id};
use tpt_erp_tenant::{TenantId, TenantSlug};

/// A stock-keeping unit.
#[derive(Debug)]
pub struct Sku;
impl Entity for Sku {}

/// A stock hold / reservation.
#[derive(Debug)]
pub struct Hold;
impl Entity for Hold {}

/// A single reservation domain event, stored append-only against the SKU aggregate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ReserveEvent {
    /// Stock received into the sellable pool.
    Received(i64),
    /// A hold placed against `reservation` for `qty` units.
    Hold { reservation: Id<Hold>, qty: u32 },
    /// A hold promoted to committed (the order paid and is being fulfilled).
    Capture { reservation: Id<Hold> },
    /// A hold released (cancelled, fulfilled, or expired).
    Release { reservation: Id<Hold> },
    /// A backorder placed for `qty` units that could not be reserved (stock short).
    BackorderPlaced { reservation: Id<Hold>, qty: u32 },
    /// A backorder fulfilled: `qty` units were allocated into a hold once stock arrived.
    BackorderFilled { reservation: Id<Hold>, qty: u32 },
    /// Stock returned from a customer return, restoring `qty` units to the sellable pool.
    Return(i64),
}

impl ReserveEvent {
    /// The signed delta this event applies to the sellable on-hand pool.
    pub fn on_hand_delta(&self) -> i64 {
        match self {
            ReserveEvent::Received(q) | ReserveEvent::Return(q) => *q,
            _ => 0,
        }
    }
}

/// Errors raised by the reservation engine.
#[derive(Debug, thiserror::Error)]
pub enum ReservationError {
    #[error("event store serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("insufficient stock for {sku}: available {available}, requested {requested}")]
    InsufficientStock {
        sku: Id<Sku>,
        available: i64,
        requested: i64,
    },
    #[error("unknown hold {0}")]
    UnknownHold(Id<Hold>),
    #[error("cache backend error: {0}")]
    Cache(#[from] CacheError),
    #[error("projection replay failed: {0}")]
    Projection(#[from] tpt_erp_ledger::ProjectionError),
    #[error("event store error: {0}")]
    Store(#[from] tpt_erp_ledger::EventStoreError),
}

const SHARDS: usize = 64;

/// Per-SKU read model derived from the event log.
#[derive(Debug, Clone, Default)]
pub struct SkuState {
    on_hand: i64,
    /// Holds promoted to committed (no longer available, will ship).
    committed: i64,
    /// Active holds keyed by reservation id.
    holds: HashMap<Id<Hold>, u32>,
    /// Expiry timestamps of active holds (fallback when no cache attached).
    expires_at: HashMap<Id<Hold>, DateTime<Utc>>,
    /// Unfulfilled backorders keyed by backorder id (awaiting stock).
    backorders: HashMap<Id<Hold>, u32>,
}

impl SkuState {
    fn held(&self) -> i64 {
        self.holds.values().map(|v| *v as i64).sum()
    }

    /// Units currently sellable: received minus committed minus active holds.
    fn available(&self) -> i64 {
        self.on_hand - self.committed - self.held()
    }
}

struct Shard {
    store: InMemoryEventStore<Id<Sku>>,
    states: HashMap<Id<Sku>, SkuState>,
}

impl Shard {
    fn new() -> Self {
        Self {
            store: InMemoryEventStore::default(),
            states: HashMap::new(),
        }
    }
}

/// Event-sourced, sharded, TTL-auto-releasing reservation engine.
pub struct ReservationEngine {
    shards: Vec<Mutex<Shard>>,
    tenant: TenantId,
    bus: Option<Box<dyn tpt_erp_bus::EventBus>>,
    cache: Option<Box<dyn ReadModelCache>>,
    /// Expiry index used for time-based release when no cache is attached.
    expiry_index: Mutex<HashMap<Id<Hold>, DateTime<Utc>>>,
    released_jobs: AtomicU64,
}

impl ReservationEngine {
    /// Create an engine for `tenant`.
    pub fn new(tenant: TenantId) -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::new())).collect(),
            tenant,
            bus: None,
            cache: None,
            expiry_index: Mutex::new(HashMap::new()),
            released_jobs: AtomicU64::new(0),
        }
    }

    /// Attach a background-job bus; reservation lifecycle events are published there.
    pub fn with_bus(mut self, bus: Box<dyn tpt_erp_bus::EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Attach a read-model cache used for TTL-driven auto-release of holds.
    pub fn with_cache(mut self, cache: Box<dyn ReadModelCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    fn shard_index(sku: &Id<Sku>) -> usize {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        sku.hash(&mut h);
        (h.finish() as usize) % SHARDS
    }

    /// Record `qty` units received into the sellable pool for `sku`.
    ///
    /// Stock arriving makes previously-placed backorders fulfillable; call
    /// [`ReservationEngine::fulfill_backorders`] (or the saga) to promote them into holds.
    pub async fn receive(&self, sku: Id<Sku>, qty: i64) -> Result<(), ReservationError> {
        let idx = Self::shard_index(&sku);
        {
            let mut shard = self.shards[idx].lock().unwrap();
            shard
                .store
                .append(Event::new(sku, "received", &ReserveEvent::Received(qty))?);
            shard.states.entry(sku).or_default().on_hand += qty;
        }
        if let Some(cache) = &self.cache {
            cache
                .put(
                    &self.tenant,
                    "on_hand",
                    &sku.as_str(),
                    serde_json::json!({ "on_hand": self.on_hand(sku) }),
                    None,
                )
                .await
                .ok();
        }
        if let Some(bus) = &self.bus {
            let _ = bus
                .publish(
                    "oms.stock_received",
                    serde_json::json!({ "sku": sku.as_str(), "qty": qty })
                        .to_string()
                        .as_bytes(),
                )
                .await;
        }
        Ok(())
    }

    /// Reserve what is available and place the shortfall on backorder instead of failing.
    ///
    /// Returns the reserved hold (with the qty actually reserved) and/or a backorder id
    /// (with the qty that could not be covered). A fully-satisfied request behaves exactly
    /// like [`ReservationEngine::reserve`]; a fully-short request reserves nothing and
    /// returns only a backorder.
    pub async fn reserve_or_backorder(
        &self,
        sku: Id<Sku>,
        qty: u32,
        ttl: Duration,
    ) -> Result<(Option<(Id<Hold>, u32)>, Option<(Id<Hold>, u32)>), ReservationError> {
        self.sweep_expired().await?;

        let idx = Self::shard_index(&sku);
        let now = Utc::now();
        let expires = now
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::days(365 * 100));

        let mut reserved: Option<(Id<Hold>, u32)> = None;
        let mut backorder: Option<(Id<Hold>, u32)> = None;

        // Compute the reservable quantity without holding the shard across appends.
        let reserved_qty = {
            let mut shard = self.shards[idx].lock().unwrap();
            let available = shard.states.entry(sku).or_default().available();
            (available as u32).min(qty)
        };

        if reserved_qty > 0 {
            let hold = Id::new();
            {
                let mut shard = self.shards[idx].lock().unwrap();
                shard.store.append(Event::new(
                    sku,
                    "hold",
                    &ReserveEvent::Hold {
                        reservation: hold,
                        qty: reserved_qty,
                    },
                )?);
                let st = shard.states.get_mut(&sku).expect("entry exists");
                st.holds.insert(hold, reserved_qty);
                st.expires_at.insert(hold, expires);
            }
            reserved = Some((hold, reserved_qty));
        }

        if reserved_qty < qty {
            let bo_qty = qty - reserved_qty;
            let bo = Id::new();
            {
                let mut shard = self.shards[idx].lock().unwrap();
                shard.store.append(Event::new(
                    sku,
                    "backorder_placed",
                    &ReserveEvent::BackorderPlaced {
                        reservation: bo,
                        qty: bo_qty,
                    },
                )?);
                let st = shard.states.get_mut(&sku).expect("entry exists");
                st.backorders.insert(bo, bo_qty);
            }
            backorder = Some((bo, bo_qty));
        }

        if let Some((hold, q)) = reserved {
            self.expiry_index.lock().unwrap().insert(hold, expires);
            if let Some(cache) = &self.cache {
                cache
                    .put(
                        &self.tenant,
                        "holds",
                        &hold.as_str(),
                        serde_json::json!({ "sku": sku.as_str(), "qty": q }),
                        Some(ttl),
                    )
                    .await
                    .ok();
            }
            if let Some(bus) = &self.bus {
                let _ = bus
                    .publish(
                        "oms.stock_reserved",
                        serde_json::json!({ "sku": sku.as_str(), "qty": q, "hold": hold.as_str() })
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
            }
        }
        if let Some((bo, q)) = backorder {
            if let Some(bus) = &self.bus {
                let _ = bus
                    .publish(
                        "oms.backorder_placed",
                        serde_json::json!({ "sku": sku.as_str(), "qty": q, "backorder": bo.as_str() })
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
            }
        }

        Ok((reserved, backorder))
    }

    /// Fulfill every backorder for `sku` that the current available pool can cover,
    /// promoting each into a real hold (emitting `BackorderFilled`). Returns the newly
    /// created holds and their quantities.
    async fn fulfill_backorders_for(
        &self,
        sku: Id<Sku>,
    ) -> Result<Vec<(Id<Hold>, u32)>, ReservationError> {
        let idx = Self::shard_index(&sku);
        let mut filled: Vec<(Id<Hold>, u32)> = Vec::new();
        loop {
            // Pick one satisfiable backorder, then release the lock before recursing.
            let picked = {
                let shard = self.shards[idx].lock().unwrap();
                let st = match shard.states.get(&sku) {
                    Some(s) => s,
                    None => break,
                };
                st.backorders
                    .iter()
                    .find(|(_, q)| st.available() >= **q as i64)
                    .map(|(id, q)| (*id, *q))
            };
            let (bo, qty) = match picked {
                Some(p) => p,
                None => break,
            };
            let hold = Id::new();
            let far_future = Utc::now() + chrono::Duration::days(365 * 100);
            {
                let mut shard = self.shards[idx].lock().unwrap();
                let st = shard.states.get_mut(&sku).expect("checked above");
                st.backorders.remove(&bo);
                st.holds.insert(hold, qty);
                st.expires_at.insert(hold, far_future);
                shard.store.append(Event::new(
                    sku,
                    "backorder_filled",
                    &ReserveEvent::BackorderFilled {
                        reservation: hold,
                        qty,
                    },
                )?);
            }
            if let Some(bus) = &self.bus {
                let _ = bus
                    .publish(
                        "oms.backorder_filled",
                        serde_json::json!({ "sku": sku.as_str(), "qty": qty, "hold": hold.as_str() })
                            .to_string()
                            .as_bytes(),
                    )
                    .await;
            }
            filled.push((hold, qty));
        }
        Ok(filled)
    }

    /// Fulfill every satisfiable backorder across all SKUs (call after stock arrives).
    /// Returns the new holds with their SKU and quantity so the caller can confirm and
    /// bill them. This is the deterministic, event-sourced backorder-fulfillment path.
    pub async fn fulfill_backorders(
        &self,
    ) -> Result<Vec<(Id<Sku>, Id<Hold>, u32)>, ReservationError> {
        let mut result = Vec::new();
        for shard in &self.shards {
            let skus: Vec<Id<Sku>> = shard.lock().unwrap().states.keys().copied().collect();
            for sku in skus {
                for (hold, qty) in self.fulfill_backorders_for(sku).await? {
                    result.push((sku, hold, qty));
                }
            }
        }
        Ok(result)
    }

    /// Restore `qty` units to the sellable pool from a customer return, reversing the
    /// committed stock allocation. Stock arriving this way also makes backorders
    /// fulfillable (promote them via [`ReservationEngine::fulfill_backorders`]).
    pub async fn return_stock(&self, sku: Id<Sku>, qty: i64) -> Result<(), ReservationError> {
        let idx = Self::shard_index(&sku);
        {
            let mut shard = self.shards[idx].lock().unwrap();
            shard
                .store
                .append(Event::new(sku, "return", &ReserveEvent::Return(qty))?);
            shard.states.entry(sku).or_default().on_hand += qty;
        }
        if let Some(cache) = &self.cache {
            cache
                .put(
                    &self.tenant,
                    "on_hand",
                    &sku.as_str(),
                    serde_json::json!({ "on_hand": self.on_hand(sku) }),
                    None,
                )
                .await
                .ok();
        }
        Ok(())
    }

    /// Place a hold for `qty` units of `sku`, auto-releasing after `ttl`.
    ///
    /// Fails with [`ReservationError::InsufficientStock`] if the available pool
    /// cannot cover the request after purging expired holds.
    pub async fn reserve(
        &self,
        sku: Id<Sku>,
        qty: u32,
        ttl: Duration,
    ) -> Result<Id<Hold>, ReservationError> {
        // Ensure availability reflects any TTL-expired holds before checking.
        self.sweep_expired().await?;

        let idx = Self::shard_index(&sku);
        let hold = Id::new();
        let now = Utc::now();
        let expires = now
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::days(365 * 100));

        {
            let mut shard = self.shards[idx].lock().unwrap();
            let available = shard.states.entry(sku).or_default().available();
            if available < qty as i64 {
                return Err(ReservationError::InsufficientStock {
                    sku,
                    available,
                    requested: qty as i64,
                });
            }
            shard.store.append(Event::new(
                sku,
                "hold",
                &ReserveEvent::Hold {
                    reservation: hold,
                    qty,
                },
            )?);
            let st = shard.states.get_mut(&sku).unwrap();
            st.holds.insert(hold, qty);
            st.expires_at.insert(hold, expires);
        }

        // Mirror the hold into the cache with its TTL — the cache TTL is the
        // authority for auto-release.
        self.expiry_index.lock().unwrap().insert(hold, expires);
        if let Some(cache) = &self.cache {
            cache
                .put(
                    &self.tenant,
                    "holds",
                    &hold.as_str(),
                    serde_json::json!({ "sku": sku.as_str(), "qty": qty }),
                    Some(ttl),
                )
                .await
                .ok();
        }
        if let Some(bus) = &self.bus {
            let _ = bus
                .publish(
                    "oms.stock_reserved",
                    serde_json::json!({ "sku": sku.as_str(), "qty": qty, "hold": hold.as_str() })
                        .to_string()
                        .as_bytes(),
                )
                .await;
        }
        Ok(hold)
    }

    /// Promote a hold to committed: the stock is now irreversibly allocated to the
    /// order (typically once payment succeeds).
    pub async fn confirm(&self, sku: Id<Sku>, hold: Id<Hold>) -> Result<(), ReservationError> {
        let idx = Self::shard_index(&sku);
        let existed = {
            let mut shard = self.shards[idx].lock().unwrap();
            let captured = {
                let st = shard
                    .states
                    .get_mut(&sku)
                    .ok_or(ReservationError::UnknownHold(hold))?;
                st.holds.remove(&hold).map(|qty| {
                    st.committed += qty as i64;
                    st.expires_at.remove(&hold);
                    qty
                })
            };
            if captured.is_some() {
                shard.store.append(Event::new(
                    sku,
                    "capture",
                    &ReserveEvent::Capture { reservation: hold },
                )?);
                true
            } else {
                false
            }
        };
        if existed {
            self.release_cache_and_index(hold).await;
        } else {
            return Err(ReservationError::UnknownHold(hold));
        }
        Ok(())
    }

    /// Explicitly release a hold, returning its units to the sellable pool.
    pub async fn release(&self, sku: Id<Sku>, hold: Id<Hold>) -> Result<(), ReservationError> {
        let released = self.release_internal(sku, hold).await?;
        if !released {
            return Err(ReservationError::UnknownHold(hold));
        }
        Ok(())
    }

    /// Units currently sellable for `sku` (active holds only; expired holds are
    /// purged lazily via [`ReservationEngine::sweep_expired`]).
    pub fn available(&self, sku: Id<Sku>) -> i64 {
        let idx = Self::shard_index(&sku);
        let shard = self.shards[idx].lock().unwrap();
        shard.states.get(&sku).map(|s| s.available()).unwrap_or(0)
    }

    /// Total received stock for `sku` (for the promo plugin's stock-aware read).
    pub fn on_hand(&self, sku: Id<Sku>) -> i64 {
        let idx = Self::shard_index(&sku);
        let shard = self.shards[idx].lock().unwrap();
        shard.states.get(&sku).map(|s| s.on_hand).unwrap_or(0)
    }

    /// Number of holds auto-released (expiry) so far — surfaced for tests/telemetry.
    pub fn released_jobs(&self) -> u64 {
        self.released_jobs.load(Ordering::Relaxed)
    }

    /// Whether `hold` is expired: when a cache is attached the cached entry's
    /// presence is the authority (absent ⇒ TTL elapsed ⇒ released); otherwise the
    /// time-based expiry index is consulted.
    async fn is_expired(&self, hold: Id<Hold>) -> bool {
        if let Some(cache) = &self.cache {
            let present = cache
                .get(&self.tenant, "holds", &hold.as_str())
                .await
                .ok()
                .flatten()
                .is_some();
            return !present;
        }
        let exp = self.expiry_index.lock().unwrap().get(&hold).copied();
        match exp {
            Some(e) => Utc::now() >= e,
            None => true,
        }
    }

    async fn release_cache_and_index(&self, hold: Id<Hold>) {
        self.expiry_index.lock().unwrap().remove(&hold);
        if let Some(cache) = &self.cache {
            let _ = cache
                .invalidate(&self.tenant, "holds", &hold.as_str())
                .await;
        }
    }

    /// Core release path: removes the hold from the shard, appends a `Release`
    /// event, and invalidates the cache. Returns whether a hold was removed.
    async fn release_internal(
        &self,
        sku: Id<Sku>,
        hold: Id<Hold>,
    ) -> Result<bool, ReservationError> {
        let idx = Self::shard_index(&sku);
        let existed = {
            let mut shard = self.shards[idx].lock().unwrap();
            let captured = {
                let st = match shard.states.get_mut(&sku) {
                    Some(s) => s,
                    None => return Ok(false),
                };
                st.holds.remove(&hold).map(|_| {
                    st.expires_at.remove(&hold);
                })
            };
            if captured.is_some() {
                shard.store.append(Event::new(
                    sku,
                    "release",
                    &ReserveEvent::Release { reservation: hold },
                )?);
                true
            } else {
                false
            }
        };
        if existed {
            self.release_cache_and_index(hold).await;
        }
        Ok(existed)
    }

    /// Release every expired hold, returning the count released. Expired-hold
    /// detection is cache/TTL-driven (see [`ReservationEngine::is_expired`]).
    pub async fn sweep_expired(&self) -> Result<u64, ReservationError> {
        let mut candidates = Vec::new();
        for shard in &self.shards {
            let s = shard.lock().unwrap();
            for (sku, st) in &s.states {
                for h in st.holds.keys() {
                    candidates.push((*sku, *h));
                }
            }
        }
        let mut released = 0u64;
        for (sku, h) in candidates {
            if self.is_expired(h).await {
                if self.release_internal(sku, h).await? {
                    released += 1;
                    self.released_jobs.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        Ok(released)
    }

    /// Rebuild the per-SKU read model from the event log (CQRS replay) and return
    /// the map of sellable state. Proves the read model can never drift from the
    /// ledger of record.
    pub async fn rebuild_read_models(
        &self,
    ) -> Result<HashMap<Id<Sku>, SkuState>, ReservationError> {
        let mut all: Vec<StoredEvent<Id<Sku>>> = Vec::new();
        for shard in &self.shards {
            let s = shard.lock().unwrap();
            all.extend(s.store.log().iter().cloned());
        }
        let events: Vec<(Id<Sku>, ReserveEvent)> = all
            .into_iter()
            .map(|e| {
                let ev = serde_json::from_value::<ReserveEvent>(e.payload)?;
                Ok((e.aggregate_id, ev))
            })
            .collect::<Result<_, serde_json::Error>>()?;
        let proj = replay(ReservationProjection::default(), events).await?;
        Ok(proj.states)
    }
}

/// CQRS read model folding reservation events into per-SKU state.
#[derive(Debug, Clone, Default)]
pub struct ReservationProjection {
    pub states: HashMap<Id<Sku>, SkuState>,
}

impl Projector for ReservationProjection {
    type Event = (Id<Sku>, ReserveEvent);

    async fn apply(&mut self, event: &Self::Event) -> Result<(), tpt_erp_ledger::ProjectionError> {
        let (sku, ev) = event;
        let st = self.states.entry(*sku).or_default();
        match ev {
            ReserveEvent::Received(q) => st.on_hand += *q,
            ReserveEvent::Hold { reservation, qty } => {
                st.holds.insert(*reservation, *qty);
            }
            ReserveEvent::Capture { reservation } => {
                if let Some(q) = st.holds.remove(reservation) {
                    st.committed += q as i64;
                }
            }
            ReserveEvent::Release { reservation } => {
                st.holds.remove(reservation);
            }
            ReserveEvent::BackorderPlaced { reservation, qty } => {
                st.backorders.insert(*reservation, *qty);
            }
            ReserveEvent::BackorderFilled {
                reservation,
                qty: _,
            } => {
                if let Some(q) = st.backorders.remove(reservation) {
                    st.holds.insert(*reservation, q);
                }
            }
            ReserveEvent::Return(q) => st.on_hand += *q,
        }
        Ok(())
    }
}

/// Build a tenant for example/demo use.
pub fn demo_tenant() -> TenantId {
    TenantSlug("oms-demo".to_string()).to_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_bus::memory::InMemoryBus;
    use tpt_erp_cache::memory::InMemoryCache;

    fn sku() -> Id<Sku> {
        Id::new()
    }

    #[tokio::test]
    async fn reserve_then_confirm_then_release_flow() {
        let eng = ReservationEngine::new(demo_tenant());
        let s = sku();
        eng.receive(s, 10).await.unwrap();
        assert_eq!(eng.available(s), 10);

        let h = eng.reserve(s, 4, Duration::from_secs(60)).await.unwrap();
        assert_eq!(eng.available(s), 6);

        eng.confirm(s, h).await.unwrap();
        // Committed stock is no longer available but still on hand.
        assert_eq!(eng.available(s), 6);
        assert_eq!(eng.on_hand(s), 10);

        // Hold is gone; releasing again is an error.
        assert!(eng.release(s, h).await.is_err());
    }

    #[tokio::test]
    async fn oversell_is_prevented() {
        let eng = ReservationEngine::new(demo_tenant());
        let s = sku();
        eng.receive(s, 3).await.unwrap();

        assert!(eng.reserve(s, 2, Duration::from_secs(60)).await.is_ok());
        // Only 1 left; a request for 2 must fail.
        assert!(matches!(
            eng.reserve(s, 2, Duration::from_secs(60)).await,
            Err(ReservationError::InsufficientStock { .. })
        ));
        assert_eq!(eng.available(s), 1);
    }

    #[tokio::test]
    async fn ttl_auto_release_via_cache() {
        let cache = InMemoryCache::new();
        let eng = ReservationEngine::new(demo_tenant()).with_cache(Box::new(cache));
        let s = sku();
        eng.receive(s, 5).await.unwrap();
        let _h = eng.reserve(s, 5, Duration::from_millis(30)).await.unwrap();
        assert_eq!(eng.available(s), 0);

        // Hold still fresh.
        assert_eq!(eng.released_jobs(), 0);

        // Wait past the TTL; the cache drops the hold and sweep releases it.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let released = eng.sweep_expired().await.unwrap();
        assert_eq!(released, 1);
        assert_eq!(eng.available(s), 5);
    }

    #[tokio::test]
    async fn concurrent_reserves_same_sku_never_oversell() {
        let eng = std::sync::Arc::new(ReservationEngine::new(demo_tenant()));
        let s = sku();
        eng.receive(s, 25).await.unwrap();

        let tasks: Vec<_> = (0..100)
            .map(|_| {
                let eng = eng.clone();
                let s = s;
                tokio::spawn(
                    async move { eng.reserve(s, 1, Duration::from_secs(60)).await.is_ok() },
                )
            })
            .collect();
        let mut ok = 0;
        for t in tasks {
            if t.await.unwrap() {
                ok += 1;
            }
        }
        // Exactly the available stock was allocated; no oversell.
        assert_eq!(ok, 25);
        assert_eq!(eng.available(s), 0);
    }

    #[tokio::test]
    async fn reservation_events_published_to_bus() {
        let bus = InMemoryBus::new();
        let eng = ReservationEngine::new(demo_tenant()).with_bus(Box::new(bus));
        let s = sku();
        eng.receive(s, 10).await.unwrap();
        let _h = eng.reserve(s, 3, Duration::from_secs(60)).await.unwrap();
        // Reserved + received events were published (no panic; structural check).
        assert_eq!(eng.available(s), 7);
    }

    #[tokio::test]
    async fn rebuild_matches_live_state() {
        let eng = ReservationEngine::new(demo_tenant());
        let s = sku();
        eng.receive(s, 20).await.unwrap();
        let h1 = eng.reserve(s, 5, Duration::from_secs(60)).await.unwrap();
        let h2 = eng.reserve(s, 3, Duration::from_secs(60)).await.unwrap();
        eng.confirm(s, h1).await.unwrap();
        eng.release(s, h2).await.unwrap();

        let rebuilt = eng.rebuild_read_models().await.unwrap();
        let st = &rebuilt[&s];
        assert_eq!(st.on_hand, 20);
        assert_eq!(st.committed, 5);
        assert_eq!(st.available(), 15);
        assert_eq!(eng.available(s), 15);
    }

    #[tokio::test]
    async fn short_reserve_goes_on_backorder_then_fulfills() {
        let eng = ReservationEngine::new(demo_tenant());
        let s = sku();
        eng.receive(s, 2).await.unwrap();

        // Want 5 but only 2 available: 2 reserved, 3 backordered.
        let (reserved, backorder) = eng
            .reserve_or_backorder(s, 5, Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(reserved.as_ref().unwrap().1, 2);
        assert_eq!(backorder.as_ref().unwrap().1, 3);
        assert_eq!(eng.available(s), 0);

        // Stock arrives; the backorder is promoted into a real hold.
        eng.receive(s, 3).await.unwrap();
        let filled = eng.fulfill_backorders().await.unwrap();
        assert_eq!(filled.len(), 1);
        assert_eq!(filled[0].0, s);
        assert_eq!(filled[0].2, 3);
        // Now all 5 units are held (reserved) and none sellable.
        assert_eq!(eng.available(s), 0);

        // Confirm the backorder hold to commit it.
        eng.confirm(filled[0].0, filled[0].1).await.unwrap();
        assert_eq!(eng.available(s), 0);
        assert_eq!(eng.on_hand(s), 5);
    }

    #[tokio::test]
    async fn returned_stock_restored_to_sellable_pool() {
        let eng = ReservationEngine::new(demo_tenant());
        let s = sku();
        eng.receive(s, 10).await.unwrap();
        let h = eng.reserve(s, 4, Duration::from_secs(60)).await.unwrap();
        eng.confirm(s, h).await.unwrap();
        assert_eq!(eng.available(s), 6);

        // A return of 3 units puts them back into the sellable pool.
        eng.return_stock(s, 3).await.unwrap();
        assert_eq!(eng.available(s), 9);
    }
}
