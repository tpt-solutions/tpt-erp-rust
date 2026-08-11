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

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tpt_erp_cache::{CacheError, ReadModelCache};
use tpt_erp_ledger::{Event, EventStore, InMemoryEventStore, Projector, StoredEvent, replay};
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

/// A License Plate Number (LPN) / pallet container entity.
#[derive(Debug)]
pub struct Lpn;
impl Entity for Lpn {}

/// Strong identifier for an LPN / pallet.
pub type LpnId = Id<Lpn>;

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
    /// A reason-coded adjustment emitted by reconciliation workflows (e.g. cycle-count).
    /// The signed `delta` is the change applied to on-hand; `reason` is audit metadata.
    Adjustment { delta: i64, reason: Reason },
}

impl Movement {
    /// The signed delta this movement applies to on-hand quantity.
    pub fn delta(&self) -> i64 {
        match self {
            Movement::Received(q) => i64::from(*q),
            Movement::Picked(q) => -(i64::from(*q)),
            Movement::Adjusted(d) => *d,
            Movement::Adjustment { delta, .. } => *delta,
        }
    }
}

/// Reason codes carried by reason-coded adjustments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    /// Initial receipt into stock.
    Receipt,
    /// Pick/issue against a sales or transfer order.
    Pick,
    /// Relocation of a pallet/LPN between bins.
    Transfer,
    /// Variance discovered during a cycle count (shrinkage or found).
    CycleCount,
    /// Damaged / spoiled goods removed from stock.
    Damage,
    /// Stock located that was not in the system.
    Found,
    /// Manual correction by an operator.
    Manual,
}

/// A lot number (human-readable batch code, e.g. "LOT-2026-0042").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct LotNumber(pub String);

impl LotNumber {
    pub fn new(s: impl Into<String>) -> Self {
        LotNumber(s.into())
    }
}

impl fmt::Display for LotNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A per-unit serial number (e.g. "SN-000123").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct SerialNumber(pub String);

impl SerialNumber {
    pub fn new(s: impl Into<String>) -> Self {
        SerialNumber(s.into())
    }
}

impl fmt::Display for SerialNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The on-hand quantity of a single lot at a bin, with optional expiry and serials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LotDetail {
    pub lot: LotNumber,
    pub expiry: Option<NaiveDate>,
    pub on_hand: u32,
    pub serials: Vec<SerialNumber>,
}

/// A lot-level event stored alongside the aggregate [`Movement`] so the lot ledger is
/// itself event-sourced and rebuildable from the same log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LotEvent {
    /// Quantity (and optionally serials) received against a lot.
    Received {
        lot: LotNumber,
        qty: u32,
        expiry: Option<NaiveDate>,
        serials: Vec<SerialNumber>,
    },
    /// Quantity (and its serials) picked/consumed from a lot.
    Picked {
        lot: LotNumber,
        qty: u32,
        serials: Vec<SerialNumber>,
    },
    /// Quantity relocated from one bin to another (pallet move). Stored under the
    /// source key; the destination records its own `Received` event.
    Transferred {
        lot: LotNumber,
        qty: u32,
        from_bin: Id<Bin>,
        to_bin: Id<Bin>,
        serials: Vec<SerialNumber>,
    },
}

/// Result of receiving a lot: the running on-hand after the receipt.
#[derive(Debug, Clone)]
pub struct LotReceipt {
    pub key: StockKey,
    pub lot: LotNumber,
    pub qty: u32,
    pub expiry: Option<NaiveDate>,
    pub on_hand: i64,
}

/// One lot's share of a pick, including any serials consumed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotConsumption {
    pub lot: LotNumber,
    pub qty: u32,
    pub serials: Vec<SerialNumber>,
}

/// A line on a pallet/LPN: a quantity of one SKU within one lot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalletLine {
    pub sku: Id<Sku>,
    pub lot: LotNumber,
    pub qty: u32,
}

/// A pallet / License Plate Number container grouping stock at a bin location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pallet {
    pub id: LpnId,
    pub location: Id<Bin>,
    pub lines: Vec<PalletLine>,
}

/// A generated cycle-count task for a single bin.
#[derive(Debug, Clone)]
pub struct CountTask {
    pub bin: Id<Bin>,
    pub lines: Vec<Id<Sku>>,
}

/// The outcome of reconciling one counted line.
#[derive(Debug, Clone)]
pub struct AdjustmentResult {
    pub key: StockKey,
    pub expected: i64,
    pub counted: i64,
    pub delta: i64,
    pub reason: Reason,
    pub new_on_hand: i64,
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
    #[error("insufficient on-hand at {key:?}: have {have}, need {need}")]
    InsufficientStock { key: StockKey, have: i64, need: i64 },
    #[error("cannot pick from expired lot {lot} at {key:?}")]
    ExpiredLot { key: StockKey, lot: LotNumber },
    #[error("received quantity must be positive at {key:?}")]
    ZeroQuantity { key: StockKey },
    #[error("serial count {have} does not match received quantity {need} at {key:?}")]
    SerialCountMismatch { key: StockKey, have: u32, need: u32 },
    #[error("duplicate serial number {0}")]
    DuplicateSerial(SerialNumber),
    #[error("lot {lot} not found at {key:?}")]
    LotNotFound { key: StockKey, lot: LotNumber },
    #[error("cannot pick {need} from lot {lot} at {key:?}: only {have} on hand")]
    OverLotQuantity {
        key: StockKey,
        lot: LotNumber,
        have: i64,
        need: i64,
    },
    #[error("pallet {0} not found")]
    PalletNotFound(LpnId),
    #[error("pallet {lpn} line {lot} of sku has only {have}, need {need}")]
    PalletLineShort {
        lpn: LpnId,
        lot: LotNumber,
        have: u32,
        need: u32,
    },
    #[error("serial number {0} not found")]
    SerialNotFound(SerialNumber),
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
    store: InMemoryEventStore<StockKey>,
    /// Running on-hand quantity per stock key (kept in sync with the log).
    on_hand: HashMap<StockKey, i64>,
    /// Per-lot on-hand ledger for lot/serial/expiry tracking.
    lots: HashMap<StockKey, Vec<LotDetail>>,
}

impl Shard {
    fn new() -> Self {
        Self {
            store: InMemoryEventStore::default(),
            on_hand: HashMap::new(),
            lots: HashMap::new(),
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
    /// Global serial registry: which (bin, sku) lot a serial currently resides in.
    serials: Mutex<HashMap<SerialNumber, (StockKey, LotNumber)>>,
    /// Pallet / LPN container registry (the physical stock it groups is event-sourced).
    pallets: Mutex<HashMap<LpnId, Pallet>>,
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
            serials: Mutex::new(HashMap::new()),
            pallets: Mutex::new(HashMap::new()),
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
            Self::append_movement_locked(&mut shard, key, movement)?
        };

        self.after_write(key, new_qty).await;
        Ok(new_qty)
    }

    /// Append a single movement to the log under an already-held shard lock, applying the
    /// floor check first so a pick/adjust can never drive on-hand negative. Returns the new
    /// on-hand quantity. The caller must perform `after_write` once the lock is released.
    fn append_movement_locked(
        shard: &mut Shard,
        key: StockKey,
        movement: Movement,
    ) -> Result<i64, InventoryError> {
        let current = *shard.on_hand.entry(key).or_insert(0);
        let new_qty = current + movement.delta();
        // Floor check: never let a pick/adjust drive on-hand negative. This rejects the
        // movement before it is ever appended to the log, keeping the read model honest.
        if new_qty < 0 {
            return Err(InventoryError::InsufficientStock {
                key,
                have: current,
                need: -movement.delta(),
            });
        }
        let event = Event::new(key, "movement", &movement)?;
        shard.store.append(event);
        shard.on_hand.insert(key, new_qty);
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
            // Floor check is applied *before* the append, so a movement that would drive
            // on-hand negative is rejected without ever being written to the log.
            let current = *shard.on_hand.entry(key).or_insert(0);
            let new_qty = current + movement.delta();
            if new_qty < 0 {
                return Err(InventoryError::InsufficientStock {
                    key,
                    have: current,
                    need: -movement.delta(),
                });
            }
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
            shard.on_hand.insert(key, new_qty);
            new_qty
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

    // ----- Lot / Serial / Expiry tracking -------------------------------------------

    /// Receive a quantity against a lot at a bin. The quantity is appended to the bin's
    /// aggregate log (`Movement::Received`) for balance, and a lot-level [`LotEvent`] is
    /// appended so the per-lot ledger is itself event-sourced. Optionally assigns
    /// per-unit serial numbers (count must equal `qty`); duplicates are rejected.
    pub async fn receive_lot(
        &self,
        key: StockKey,
        lot: LotNumber,
        qty: u32,
        expiry: Option<NaiveDate>,
        serials: Vec<SerialNumber>,
    ) -> Result<LotReceipt, InventoryError> {
        if qty == 0 {
            return Err(InventoryError::ZeroQuantity { key });
        }
        if !serials.is_empty() && serials.len() as u32 != qty {
            return Err(InventoryError::SerialCountMismatch {
                key,
                have: serials.len() as u32,
                need: qty,
            });
        }
        // Reject duplicate serials before mutating any state.
        {
            let s = self.serials.lock().unwrap();
            for sn in &serials {
                if s.contains_key(sn) {
                    return Err(InventoryError::DuplicateSerial(sn.clone()));
                }
            }
        }

        let new_qty = {
            let idx = Self::shard_index(&key);
            let mut shard = self.shards[idx].lock().unwrap();
            let new_qty = Self::append_movement_locked(&mut shard, key, Movement::Received(qty))?;
            let lots = shard.lots.entry(key).or_default();
            if let Some(ld) = lots.iter_mut().find(|l| l.lot == lot) {
                ld.on_hand += qty;
                ld.serials.extend(serials.iter().cloned());
                if expiry.is_some() {
                    ld.expiry = expiry;
                }
            } else {
                lots.push(LotDetail {
                    lot: lot.clone(),
                    expiry,
                    on_hand: qty,
                    serials: serials.clone(),
                });
            }
            let ev = Event::new(
                key,
                "lot",
                &LotEvent::Received {
                    lot: lot.clone(),
                    qty,
                    expiry,
                    serials: serials.clone(),
                },
            )?;
            shard.store.append(ev);
            new_qty
        };

        if !serials.is_empty() {
            let mut s = self.serials.lock().unwrap();
            for sn in &serials {
                s.insert(sn.clone(), (key, lot.clone()));
            }
        }

        self.after_write(key, new_qty).await;
        Ok(LotReceipt {
            key,
            lot,
            qty,
            expiry,
            on_hand: new_qty,
        })
    }

    /// Pick `qty` from a bin, consuming lots FIFO by nearest expiry first (FEFO). Expired
    /// lots are never issued: if only expired stock is available the whole pick is
    /// rejected; otherwise non-expired lots are consumed. Picking more than the available
    /// (non-expired) on-hand is rejected, and the per-lot floor check is preserved.
    pub async fn pick(
        &self,
        key: StockKey,
        qty: u32,
        as_of: NaiveDate,
    ) -> Result<Vec<LotConsumption>, InventoryError> {
        if qty == 0 {
            return Err(InventoryError::ZeroQuantity { key });
        }
        let (consumptions, new_qty, picked_serials) = {
            let idx = Self::shard_index(&key);
            let mut shard = self.shards[idx].lock().unwrap();

            // 1. Plan the FEFO consumption and mutate the per-lot ledger. This borrow of
            //    `shard.lots` is scoped so it ends before we touch the movement log and the
            //    aggregate on-hand total in steps 2-3 below.
            let (mut consumptions, picked_serials) = {
                let lots = shard.lots.entry(key).or_default();
                let plan = build_pick_plan(lots, qty, as_of, key)?;
                let mut consumptions = Vec::with_capacity(plan.len());
                let mut picked_serials: Vec<SerialNumber> = Vec::new();
                for (i, take) in plan {
                    let ld = &mut lots[i];
                    let n = ld.serials.len().min(take as usize);
                    let ser: Vec<SerialNumber> = ld.serials.drain(0..n).collect();
                    ld.on_hand -= take;
                    picked_serials.extend(ser.clone());
                    consumptions.push(LotConsumption {
                        lot: ld.lot.clone(),
                        qty: take,
                        serials: ser,
                    });
                }
                (consumptions, picked_serials)
            };

            // 2. Append a movement + lot event per consumed line. The aggregate floor check
            //    is enforced here (same code path as `apply`).
            let mut new_qty = *shard.on_hand.get(&key).unwrap_or(&0);
            for c in &mut consumptions {
                new_qty = Self::append_movement_locked(&mut shard, key, Movement::Picked(c.qty))?;
                let ev = Event::new(
                    key,
                    "lot",
                    &LotEvent::Picked {
                        lot: c.lot.clone(),
                        qty: c.qty,
                        serials: c.serials.clone(),
                    },
                )?;
                shard.store.append(ev);
            }

            // 3. Drop lots whose on-hand reached zero.
            if let Some(lots) = shard.lots.get_mut(&key) {
                lots.retain(|l| l.on_hand > 0);
            }

            (consumptions, new_qty, picked_serials)
        };

        if !picked_serials.is_empty() {
            let mut s = self.serials.lock().unwrap();
            for sn in &picked_serials {
                s.remove(sn);
            }
        }

        self.after_write(key, new_qty).await;
        Ok(consumptions)
    }

    /// Picking from a specific lot: rejects if the lot is expired or over its on-hand.
    pub async fn pick_from_lot(
        &self,
        key: StockKey,
        lot: LotNumber,
        qty: u32,
        as_of: NaiveDate,
    ) -> Result<Vec<LotConsumption>, InventoryError> {
        if qty == 0 {
            return Err(InventoryError::ZeroQuantity { key });
        }
        let (consumption, new_qty, picked_serials) = {
            let idx = Self::shard_index(&key);
            let mut shard = self.shards[idx].lock().unwrap();

            // 1. Validate against the lot ledger (read-only borrow, scoped so it ends
            //    before we mutate `shard` in steps 2-3).
            let (ser, n) = {
                let lots = shard.lots.entry(key).or_default();
                let i = lots.iter().position(|l| l.lot == lot).ok_or_else(|| {
                    InventoryError::LotNotFound {
                        key,
                        lot: lot.clone(),
                    }
                })?;
                let ld = &lots[i];
                if let Some(exp) = ld.expiry {
                    if exp < as_of {
                        return Err(InventoryError::ExpiredLot {
                            key,
                            lot: lot.clone(),
                        });
                    }
                }
                if ld.on_hand < qty {
                    return Err(InventoryError::OverLotQuantity {
                        key,
                        lot: lot.clone(),
                        have: ld.on_hand as i64,
                        need: qty as i64,
                    });
                }
                let n = ld.serials.len().min(qty as usize);
                (ld.serials[..n].to_vec(), n)
            };

            // 2. Append the aggregate movement + lot event (enforces the floor check).
            let new_qty = Self::append_movement_locked(&mut shard, key, Movement::Picked(qty))?;
            let ev = Event::new(
                key,
                "lot",
                &LotEvent::Picked {
                    lot: lot.clone(),
                    qty,
                    serials: ser.clone(),
                },
            )?;
            shard.store.append(ev);

            // 3. Mutate the lot ledger now that the read borrow has ended.
            if let Some(lots) = shard.lots.get_mut(&key) {
                if let Some(ld) = lots.iter_mut().find(|l| l.lot == lot) {
                    ld.serials.drain(0..n);
                    ld.on_hand -= qty;
                }
            }

            (
                LotConsumption {
                    lot: lot.clone(),
                    qty,
                    serials: ser.clone(),
                },
                new_qty,
                ser,
            )
        };

        if !picked_serials.is_empty() {
            let mut s = self.serials.lock().unwrap();
            for sn in &picked_serials {
                s.remove(sn);
            }
        }

        self.after_write(key, new_qty).await;
        Ok(vec![consumption])
    }

    /// Snapshot of the per-lot ledger at a bin.
    pub fn lots(&self, key: StockKey) -> Vec<LotDetail> {
        let idx = Self::shard_index(&key);
        let shard = self.shards[idx].lock().unwrap();
        shard.lots.get(&key).cloned().unwrap_or_default()
    }

    /// Resolve where a serial currently resides.
    pub fn serial_location(&self, sn: &SerialNumber) -> Option<(StockKey, LotNumber)> {
        self.serials.lock().unwrap().get(sn).cloned()
    }

    // ----- Pallet / LPN model ------------------------------------------------------

    /// Register a pallet/LPN grouping quantities of one or more (sku, lot) lines at a bin.
    /// The underlying stock must already be received; the pallet is a locator over it.
    pub async fn create_pallet(
        &self,
        location: Id<Bin>,
        lines: Vec<PalletLine>,
    ) -> Result<LpnId, InventoryError> {
        let id = LpnId::new();
        for line in &lines {
            let key = StockKey {
                bin: location,
                sku: line.sku,
            };
            let idx = Self::shard_index(&key);
            let shard = self.shards[idx].lock().unwrap();
            let have = shard
                .lots
                .get(&key)
                .and_then(|ls| ls.iter().find(|l| l.lot == line.lot))
                .map(|l| l.on_hand)
                .unwrap_or(0);
            if have < line.qty {
                return Err(InventoryError::PalletLineShort {
                    lpn: id,
                    lot: line.lot.clone(),
                    have,
                    need: line.qty,
                });
            }
        }
        let pallet = Pallet {
            id,
            location,
            lines,
        };
        self.pallets.lock().unwrap().insert(id, pallet);
        Ok(id)
    }

    /// Relocate a whole pallet to another bin: every line's quantity is transferred
    /// (FIFO-expiry-respecting, lot-preserving) from the source bin to the destination
    /// bin. The pallet's contents (lines) are unchanged; only its location moves.
    pub async fn move_pallet(&self, lpn: LpnId, to_bin: Id<Bin>) -> Result<(), InventoryError> {
        let pallet = self
            .pallets
            .lock()
            .unwrap()
            .get(&lpn)
            .cloned()
            .ok_or(InventoryError::PalletNotFound(lpn))?;
        if pallet.location == to_bin {
            return Ok(());
        }
        for line in &pallet.lines {
            let from = StockKey {
                bin: pallet.location,
                sku: line.sku,
            };
            self.transfer_lot(from, line.lot.clone(), line.qty, pallet.location, to_bin)
                .await?;
        }
        if let Some(p) = self.pallets.lock().unwrap().get_mut(&lpn) {
            p.location = to_bin;
        }
        Ok(())
    }

    /// Inner pick from a specific pallet line: reduces the pallet line and issues stock
    /// from that bin's lot (FEFO, expiry-checked).
    pub async fn pick_from_pallet(
        &self,
        lpn: LpnId,
        sku: Id<Sku>,
        lot: LotNumber,
        qty: u32,
        as_of: NaiveDate,
    ) -> Result<Vec<LotConsumption>, InventoryError> {
        let location = {
            let mut ps = self.pallets.lock().unwrap();
            let p = ps.get(&lpn).ok_or(InventoryError::PalletNotFound(lpn))?;
            let line = p
                .lines
                .iter()
                .find(|l| l.sku == sku && l.lot == lot)
                .ok_or_else(|| InventoryError::LotNotFound {
                    key: StockKey {
                        bin: p.location,
                        sku,
                    },
                    lot: lot.clone(),
                })?;
            if line.qty < qty {
                return Err(InventoryError::PalletLineShort {
                    lpn,
                    lot: lot.clone(),
                    have: line.qty,
                    need: qty,
                });
            }
            let loc = p.location;
            if let Some(line) = ps
                .get_mut(&lpn)
                .unwrap()
                .lines
                .iter_mut()
                .find(|l| l.sku == sku && l.lot == lot)
            {
                line.qty -= qty;
            }
            loc
        };
        let key = StockKey { bin: location, sku };
        self.pick(key, qty, as_of).await
    }

    /// Transfer a quantity of a specific lot from one bin to another (pallet relocation).
    /// Relocation is not an issue, so expiry is not a blocker; the per-lot floor is enforced.
    async fn transfer_lot(
        &self,
        key: StockKey,
        lot: LotNumber,
        qty: u32,
        from_bin: Id<Bin>,
        to_bin: Id<Bin>,
    ) -> Result<(), InventoryError> {
        if qty == 0 {
            return Err(InventoryError::ZeroQuantity { key });
        }
        let (moved_serials, expiry) = {
            let idx = Self::shard_index(&key);
            let mut shard = self.shards[idx].lock().unwrap();
            let lots = shard
                .lots
                .get_mut(&key)
                .ok_or_else(|| InventoryError::LotNotFound {
                    key,
                    lot: lot.clone(),
                })?;
            let ld = lots.iter_mut().find(|l| l.lot == lot).ok_or_else(|| {
                InventoryError::LotNotFound {
                    key,
                    lot: lot.clone(),
                }
            })?;
            if ld.on_hand < qty {
                return Err(InventoryError::OverLotQuantity {
                    key,
                    lot: lot.clone(),
                    have: ld.on_hand as i64,
                    need: qty as i64,
                });
            }
            let n = ld.serials.len().min(qty as usize);
            let ser: Vec<SerialNumber> = ld.serials.drain(0..n).collect();
            let expiry = ld.expiry;
            ld.on_hand -= qty;
            Self::append_movement_locked(&mut shard, key, Movement::Picked(qty))?;
            let ev = Event::new(
                key,
                "lot",
                &LotEvent::Transferred {
                    lot: lot.clone(),
                    qty,
                    from_bin,
                    to_bin,
                    serials: ser.clone(),
                },
            )?;
            shard.store.append(ev);
            (ser, expiry)
        };

        let dest = StockKey {
            bin: to_bin,
            sku: key.sku,
        };
        if !moved_serials.is_empty() {
            let mut s = self.serials.lock().unwrap();
            for sn in &moved_serials {
                s.insert(sn.clone(), (dest, lot.clone()));
            }
        }
        let new_qty = {
            let idx = Self::shard_index(&dest);
            let mut shard = self.shards[idx].lock().unwrap();
            let new_qty = Self::append_movement_locked(&mut shard, dest, Movement::Received(qty))?;
            let lots = shard.lots.entry(dest).or_default();
            if let Some(ld) = lots.iter_mut().find(|l| l.lot == lot) {
                ld.on_hand += qty;
                ld.serials.extend(moved_serials.iter().cloned());
                if expiry.is_some() {
                    ld.expiry = expiry;
                }
            } else {
                lots.push(LotDetail {
                    lot: lot.clone(),
                    expiry,
                    on_hand: qty,
                    serials: moved_serials.clone(),
                });
            }
            let ev = Event::new(
                dest,
                "lot",
                &LotEvent::Received {
                    lot: lot.clone(),
                    qty,
                    expiry,
                    serials: moved_serials.clone(),
                },
            )?;
            shard.store.append(ev);
            new_qty
        };

        self.after_write(dest, new_qty).await;
        Ok(())
    }

    /// Look up a pallet by its LPN.
    pub fn pallet(&self, lpn: LpnId) -> Option<Pallet> {
        self.pallets.lock().unwrap().get(&lpn).cloned()
    }

    // ----- Cycle-count workflow ----------------------------------------------------

    /// Generate a cycle-count task for a bin: every SKU currently carried there.
    pub fn generate_count_task(&self, bin: Id<Bin>) -> CountTask {
        let mut lines = Vec::new();
        for shard in &self.shards {
            let s = shard.lock().unwrap();
            for (k, q) in &s.on_hand {
                if k.bin == bin && *q != 0 {
                    lines.push(k.sku);
                }
            }
        }
        CountTask { bin, lines }
    }

    /// Reconcile a counted task against the system of record. For every line whose counted
    /// quantity differs from on-hand, emit a reason-coded `Movement::Adjustment` through the
    /// same event-sourced path; the variance becomes the delta, so balances stay correct.
    pub async fn reconcile_count(
        &self,
        task: &CountTask,
        counted: &HashMap<StockKey, i64>,
        reason: Reason,
    ) -> Result<Vec<AdjustmentResult>, InventoryError> {
        let mut results = Vec::new();
        for &sku in &task.lines {
            let key = StockKey { bin: task.bin, sku };
            let expected = self.on_hand(key);
            let counted_qty = counted.get(&key).copied().unwrap_or(0);
            let delta = counted_qty - expected;
            if delta != 0 {
                let new_on_hand = self
                    .apply(key, Movement::Adjustment { delta, reason })
                    .await?;
                results.push(AdjustmentResult {
                    key,
                    expected,
                    counted: counted_qty,
                    delta,
                    reason,
                    new_on_hand,
                });
            }
        }
        Ok(results)
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
            .filter(|e| e.event_type == "movement")
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

    /// Rebuild the per-lot ledger from the event log (CQRS replay of `LotEvent`s). Proves
    /// the lot read model is derivable from the same ledger, with no drift.
    pub async fn rebuild_lot_read_models(
        &self,
    ) -> Result<HashMap<StockKey, Vec<LotDetail>>, InventoryError> {
        let mut all: Vec<StoredEvent<StockKey>> = Vec::new();
        for shard in &self.shards {
            let s = shard.lock().unwrap();
            all.extend(s.store.log().iter().cloned());
        }
        let events: Vec<(StockKey, LotEvent)> = all
            .into_iter()
            .filter(|e| e.event_type == "lot")
            .map(|e| {
                let le = serde_json::from_value::<LotEvent>(e.payload)?;
                Ok((e.aggregate_id, le))
            })
            .collect::<Result<_, serde_json::Error>>()?;
        let proj = replay(LotProjection::default(), events).await?;
        Ok(proj.lots)
    }
}

/// Build a FIFO-by-expiry consumption plan over the lots at a key. Expired lots (expiry
/// strictly before `as_of`) are excluded from candidates. Returns, per chosen lot, its
/// index and the quantity to take. Errors if the available non-expired stock is
/// insufficient, distinguishing "only expired stock remains" from "genuinely short".
fn build_pick_plan(
    lots: &[LotDetail],
    qty: u32,
    as_of: NaiveDate,
    key: StockKey,
) -> Result<Vec<(usize, u32)>, InventoryError> {
    // Candidate lots: not expired. Sort by expiry ascending so the nearest-expiry lot
    // is consumed first; `None` (no expiry) sorts after any concrete date.
    let mut candidates: Vec<usize> = (0..lots.len())
        .filter(|&i| lots[i].expiry.map_or(true, |e| e >= as_of))
        .collect();
    candidates.sort_by_key(|&i| lots[i].expiry);

    let mut remaining = qty;
    let mut plan = Vec::new();
    for i in candidates {
        if remaining == 0 {
            break;
        }
        let take = lots[i].on_hand.min(remaining);
        if take == 0 {
            continue;
        }
        plan.push((i, take));
        remaining -= take;
    }

    if remaining > 0 {
        let expired: Vec<&LotDetail> = lots
            .iter()
            .filter(|l| l.expiry.map_or(false, |e| e < as_of))
            .collect();
        if !expired.is_empty() {
            let lot = expired.iter().min_by_key(|l| l.expiry).unwrap().lot.clone();
            return Err(InventoryError::ExpiredLot { key, lot });
        }
        return Err(InventoryError::InsufficientStock {
            key,
            have: (qty - remaining) as i64,
            need: remaining as i64,
        });
    }
    Ok(plan)
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

/// CQRS read model for the per-lot ledger, folded from `LotEvent`s.
#[derive(Debug, Clone, Default)]
pub struct LotProjection {
    pub lots: HashMap<StockKey, Vec<LotDetail>>,
}

impl Projector for LotProjection {
    type Event = (StockKey, LotEvent);

    async fn apply(&mut self, event: &Self::Event) -> Result<(), tpt_erp_ledger::ProjectionError> {
        let (key, le) = event;
        let lots = self.lots.entry(*key).or_default();
        match le {
            LotEvent::Received {
                lot,
                qty,
                expiry,
                serials,
            } => {
                if let Some(ld) = lots.iter_mut().find(|l| l.lot == *lot) {
                    ld.on_hand += qty;
                    ld.serials.extend(serials.iter().cloned());
                    if expiry.is_some() {
                        ld.expiry = *expiry;
                    }
                } else {
                    lots.push(LotDetail {
                        lot: lot.clone(),
                        expiry: *expiry,
                        on_hand: *qty,
                        serials: serials.clone(),
                    });
                }
            }
            LotEvent::Picked { lot, qty, serials } => {
                if let Some(ld) = lots.iter_mut().find(|l| l.lot == *lot) {
                    let n = ld.serials.len().min(*qty as usize);
                    ld.serials.drain(0..n);
                    ld.on_hand = ld.on_hand.saturating_sub(*qty);
                }
                let _ = serials;
            }
            LotEvent::Transferred {
                lot, qty, serials, ..
            } => {
                // Stored under the source key: reduce the source lot.
                if let Some(ld) = lots.iter_mut().find(|l| l.lot == *lot) {
                    let n = ld.serials.len().min(*qty as usize);
                    ld.serials.drain(0..n);
                    ld.on_hand = ld.on_hand.saturating_sub(*qty);
                }
                let _ = serials;
            }
        }
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

    // ----- Lot / Serial / Expiry ------------------------------------------------

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
    }

    #[tokio::test]
    async fn fifo_consumes_nearest_expiry_first() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        let d = today();
        eng.receive_lot(
            k,
            LotNumber::new("A"),
            5,
            Some(d + Duration::days(30)),
            vec![],
        )
        .await
        .unwrap();
        eng.receive_lot(
            k,
            LotNumber::new("B"),
            5,
            Some(d + Duration::days(10)),
            vec![],
        )
        .await
        .unwrap();
        eng.receive_lot(
            k,
            LotNumber::new("C"),
            5,
            Some(d + Duration::days(20)),
            vec![],
        )
        .await
        .unwrap();

        // Picking 8 must take all of B (expiry +10d) then 3 of C (expiry +20d),
        // leaving A (expiry +30d) untouched.
        let cons = eng.pick(k, 8, d).await.unwrap();
        assert_eq!(cons.len(), 2);
        assert_eq!(cons[0].lot, LotNumber::new("B"));
        assert_eq!(cons[0].qty, 5);
        assert_eq!(cons[1].lot, LotNumber::new("C"));
        assert_eq!(cons[1].qty, 3);
        assert_eq!(eng.on_hand(k), 7);

        // The remaining A lot is intact.
        let lots = eng.lots(k);
        assert_eq!(lots.len(), 2);
        assert!(
            lots.iter()
                .any(|l| l.lot == LotNumber::new("A") && l.on_hand == 5)
        );
        assert!(
            lots.iter()
                .any(|l| l.lot == LotNumber::new("C") && l.on_hand == 2)
        );
    }

    #[tokio::test]
    async fn expired_lot_pick_rejected() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        let d = today();
        eng.receive_lot(
            k,
            LotNumber::new("OLD"),
            10,
            Some(d - Duration::days(5)),
            vec![],
        )
        .await
        .unwrap();
        let r = eng.pick(k, 1, d).await;
        assert!(matches!(r, Err(InventoryError::ExpiredLot { .. })));
        // Nothing was issued.
        assert_eq!(eng.on_hand(k), 10);
        assert_eq!(eng.lots(k)[0].on_hand, 10);
    }

    #[tokio::test]
    async fn pick_over_lot_on_hand_rejected_preserves_floor() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        let d = today();
        eng.receive_lot(k, LotNumber::new("X"), 3, None, vec![])
            .await
            .unwrap();
        let r = eng.pick(k, 5, d).await;
        assert!(matches!(r, Err(InventoryError::InsufficientStock { .. })));
        assert_eq!(eng.on_hand(k), 3);
    }

    #[tokio::test]
    async fn serial_tracking_records_and_rejects_duplicates() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        let d = today();
        let serials: Vec<SerialNumber> = (0..3)
            .map(|i| SerialNumber::new(format!("SN{i}")))
            .collect();
        eng.receive_lot(k, LotNumber::new("S"), 3, None, serials.clone())
            .await
            .unwrap();
        // Same serial on a different receipt is rejected.
        let dup = eng
            .receive_lot(
                k,
                LotNumber::new("S2"),
                1,
                None,
                vec![SerialNumber::new("SN0")],
            )
            .await;
        assert!(matches!(dup, Err(InventoryError::DuplicateSerial(_))));
        // Picking consumes the serials and removes them from the registry.
        let cons = eng.pick(k, 3, d).await.unwrap();
        assert_eq!(cons[0].serials.len(), 3);
        for sn in &serials {
            assert!(eng.serial_location(sn).is_none());
        }
    }

    #[tokio::test]
    async fn lot_ledger_rebuild_matches_live() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        let d = today();
        eng.receive_lot(
            k,
            LotNumber::new("A"),
            4,
            Some(d + Duration::days(5)),
            vec![],
        )
        .await
        .unwrap();
        eng.receive_lot(
            k,
            LotNumber::new("B"),
            6,
            Some(d + Duration::days(15)),
            vec![],
        )
        .await
        .unwrap();
        eng.pick(k, 5, d).await.unwrap(); // 4 of A gone, 1 of B gone
        let rebuilt = eng.rebuild_lot_read_models().await.unwrap();
        let lots = rebuilt.get(&k).unwrap();
        assert_eq!(
            lots.iter()
                .find(|l| l.lot == LotNumber::new("A"))
                .unwrap()
                .on_hand,
            0
        );
        assert_eq!(
            lots.iter()
                .find(|l| l.lot == LotNumber::new("B"))
                .unwrap()
                .on_hand,
            5
        );
        assert_eq!(eng.on_hand(k), 5);
    }

    // ----- Pallet / LPN ---------------------------------------------------------

    #[tokio::test]
    async fn lpn_move_keeps_contents_consistent() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let bin_a = Id::new();
        let bin_b = Id::new();
        let sku = Id::new();
        let k_a = StockKey { bin: bin_a, sku };
        eng.receive_lot(k_a, LotNumber::new("L1"), 10, None, vec![])
            .await
            .unwrap();
        let lpn = eng
            .create_pallet(
                bin_a,
                vec![PalletLine {
                    sku,
                    lot: LotNumber::new("L1"),
                    qty: 10,
                }],
            )
            .await
            .unwrap();

        eng.move_pallet(lpn, bin_b).await.unwrap();

        let p = eng.pallet(lpn).unwrap();
        assert_eq!(p.location, bin_b);
        assert_eq!(p.lines.len(), 1);
        assert_eq!(p.lines[0].qty, 10);

        // Stock physically moved: bin A empty, bin B holds the same 10.
        assert_eq!(eng.on_hand(k_a), 0);
        assert_eq!(eng.on_hand(StockKey { bin: bin_b, sku }), 10);
        // The lot carries over to the destination bin.
        assert!(
            eng.lots(StockKey { bin: bin_b, sku })
                .iter()
                .any(|l| l.lot == LotNumber::new("L1") && l.on_hand == 10)
        );
    }

    #[tokio::test]
    async fn lpn_inner_pick_reduces_line_and_stock() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let bin = Id::new();
        let sku = Id::new();
        let k = StockKey { bin, sku };
        let d = today();
        eng.receive_lot(k, LotNumber::new("L1"), 10, None, vec![])
            .await
            .unwrap();
        let lpn = eng
            .create_pallet(
                bin,
                vec![PalletLine {
                    sku,
                    lot: LotNumber::new("L1"),
                    qty: 10,
                }],
            )
            .await
            .unwrap();

        eng.pick_from_pallet(lpn, sku, LotNumber::new("L1"), 4, d)
            .await
            .unwrap();

        let p = eng.pallet(lpn).unwrap();
        assert_eq!(p.lines[0].qty, 6);
        assert_eq!(eng.on_hand(k), 6);
    }

    // ----- Cycle count ----------------------------------------------------------

    #[tokio::test]
    async fn cycle_count_reconciles_variance_keeping_balance() {
        let eng = InventoryEngine::new(demo_tenant(), 0);
        let k = key();
        eng.apply(k, Movement::Received(50)).await.unwrap();

        let task = eng.generate_count_task(k.bin);
        let mut counted = HashMap::new();
        counted.insert(k, 45); // shrinkage of 5
        let adj = eng
            .reconcile_count(&task, &counted, Reason::CycleCount)
            .await
            .unwrap();
        assert_eq!(adj.len(), 1);
        assert_eq!(adj[0].expected, 50);
        assert_eq!(adj[0].counted, 45);
        assert_eq!(adj[0].delta, -5);
        assert_eq!(eng.on_hand(k), 45);

        // A later "found" count brings it back up.
        let mut found = HashMap::new();
        found.insert(k, 50);
        let adj2 = eng
            .reconcile_count(&eng.generate_count_task(k.bin), &found, Reason::Found)
            .await
            .unwrap();
        assert_eq!(adj2[0].delta, 5);
        assert_eq!(eng.on_hand(k), 50);

        // Variance reconciliation survives a full rebuild.
        let rebuilt = eng.rebuild_read_models().await.unwrap();
        assert_eq!(rebuilt[&k], 50);
    }
}
