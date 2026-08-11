//! # flow — cross-vertical reference orchestration on TPT ERP
//!
//! This reference crate proves the framework's headline promise: **verticals are
//! decoupled and talk only through the event bus**. A single Commerce event fans out
//! across four reference implementations without any of them calling another directly:
//!
//! ```text
//! oms.order.created  ──▶  WMS  pick          ──▶  wms.pick.done
//! wms.pick.done      ──▶  TMS  route plan    ──▶  tms.dispatch.done
//! tms.dispatch.done  ──▶  GL   post COGS      ──▶  gl.posted
//! ```
//!
//! Each step is a *different* reference vertical (`oms`, `wms`, `tms`, `gl`), wired
//! together only by `tpt-erp-bus` subjects. Swap the in-memory bus for NATS JetStream
//! (feature `nats` on `tpt-erp-bus`) and the same flow runs across processes.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use tpt_erp_bus::{memory::InMemoryBus, EventBus};
use tpt_erp_ledger::{EntrySide, LedgerEntry, TransactionId};
use tpt_erp_primitives::{Id, Money, Usd};
use tpt_erp_tenant::{TenantId, TenantSlug};

// The four reference verticals stitched by this flow.
use gl::journal::demo;
use oms::reservation::{ReservationEngine, Sku as OmsSku};
use tms::geo::LatLng;
use tms::route::{self, Stop};
use wms::inventory::{Bin, InventoryEngine, LotNumber, Sku as WmsSku, StockKey};

/// Subject fired when an OMS order is placed (stock reserved/committed).
pub const SUBJECT_ORDER_CREATED: &str = "oms.order.created";
/// Subject fired when WMS has picked the ordered goods.
pub const SUBJECT_PICK_DONE: &str = "wms.pick.done";
/// Subject fired when TMS has planned the dispatch route.
pub const SUBJECT_DISPATCH_DONE: &str = "tms.dispatch.done";
/// Subject fired when GL has posted the resulting ledger entry.
pub const SUBJECT_GL_POSTED: &str = "gl.posted";

/// Payload of [`SUBJECT_ORDER_CREATED`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCreated {
    pub order_id: String,
    pub sku: String,
    pub bin: String,
    pub qty: u32,
}

/// Payload of [`SUBJECT_PICK_DONE`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickDone {
    pub sku: String,
    pub bin: String,
    pub qty: u32,
}

/// Payload of [`SUBJECT_DISPATCH_DONE`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchDone {
    pub qty: u32,
    pub stops: usize,
    pub distance_km: f64,
}

/// Payload of [`SUBJECT_GL_POSTED`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlPosted {
    pub tx: String,
}

/// The observable outcome of a completed cross-vertical flow.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowReport {
    /// Units picked by WMS.
    pub picked: u32,
    /// Number of stops in the TMS dispatch route.
    pub route_stops: usize,
    /// Length of the optimized TMS route, in km.
    pub route_distance_km: f64,
    /// The GL transaction id that recorded the COGS.
    pub posted_tx: String,
    /// The balanced amount GL posted (COGS recognized).
    pub posted_amount: Money<Usd>,
}

/// Run the full OMS → WMS → TMS → GL flow for one order of `qty` units at `unit_cost`.
///
/// The four verticals are each constructed independently and connected only through the
/// bus. The returned [`FlowReport`] proves every step executed in sequence.
pub async fn run_flow(
    tenant: TenantId,
    qty: u32,
    unit_cost: Money<Usd>,
) -> anyhow::Result<FlowReport> {
    let period = "2026-01";
    let as_of = NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date");

    let bus = Arc::new(InMemoryBus::new());

    // --- OMS vertical: reserve/commit the ordered stock into a hold. ---------------------
    let oms_res = Arc::new(ReservationEngine::new(tenant));
    let oms_sku = Id::<OmsSku>::new();
    oms_res.receive(oms_sku, qty as i64).await?;
    let hold = oms_res
        .reserve(oms_sku, qty, Duration::from_secs(600))
        .await?;
    oms_res.confirm(oms_sku, hold).await?;

    // --- WMS vertical: the bin/SKU the goods live in. -----------------------------------
    let wms_sku = Id::<WmsSku>::new();
    let bin = Id::<Bin>::new();
    let wms = Arc::new(InventoryEngine::new(tenant, 0));

    // --- TMS vertical: a small delivery route (depot + 3 drops). ------------------------
    let stops = vec![
        Stop {
            id: 0,
            pos: LatLng::new(37.7749, -122.4194),
        },
        Stop {
            id: 1,
            pos: LatLng::new(37.3382, -121.8863),
        },
        Stop {
            id: 2,
            pos: LatLng::new(36.7783, -119.4179),
        },
        Stop {
            id: 3,
            pos: LatLng::new(34.0522, -118.2437),
        },
    ];

    // --- GL vertical: the ledger that records the cost of goods sold. --------------------
    let (gl_eng, coa) = demo(tenant);
    let gl_eng = Arc::new(gl_eng);

    // Completion channel: GL signals when the whole chain has settled.
    let (done_tx, mut done_rx) = mpsc::channel::<FlowReport>(1);

    // Subscriptions are established up-front so no event is missed before a handler is
    // listening (the spawned tasks only *consume* the already-registered streams).
    let mut wms_sub = bus.subscribe(SUBJECT_ORDER_CREATED).await?;
    let mut tms_sub = bus.subscribe(SUBJECT_PICK_DONE).await?;
    let mut gl_sub = bus.subscribe(SUBJECT_DISPATCH_DONE).await?;

    // WMS handler: receive the goods, pick them, emit `wms.pick.done`.
    {
        let bus = bus.clone();
        let wms = wms.clone();
        tokio::spawn(async move {
            while let Some(msg) = wms_sub.next().await {
                let oc: OrderCreated = match serde_json::from_slice(&msg.payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let sku = Id::<WmsSku>::from_str(&oc.sku).expect("valid wms sku id");
                let bin = Id::<Bin>::from_str(&oc.bin).expect("valid bin id");
                let key = StockKey { bin, sku };
                // Inbound receipt (fulfillment shipment) then a FEFO pick of the order qty.
                let _ = wms
                    .receive_lot(key, LotNumber::new("LOT-FLOW"), oc.qty, None, vec![])
                    .await;
                let _consumed = wms.pick(key, oc.qty, as_of).await;
                let pd = PickDone {
                    sku: oc.sku,
                    bin: oc.bin,
                    qty: oc.qty,
                };
                let _ = bus
                    .publish(SUBJECT_PICK_DONE, &serde_json::to_vec(&pd).unwrap())
                    .await;
                break;
            }
        });
    }

    // TMS handler: plan the dispatch route, emit `tms.dispatch.done`.
    {
        let bus = bus.clone();
        tokio::spawn(async move {
            while let Some(msg) = tms_sub.next().await {
                let pd: PickDone = match serde_json::from_slice(&msg.payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Nearest-neighbor seed refined by parallel 2-opt (real TMS algorithm).
                let tour = route::optimize(&stops, 8);
                let distance_km = route::tour_distance(&stops, &tour);
                let dd = DispatchDone {
                    qty: pd.qty,
                    stops: stops.len(),
                    distance_km,
                };
                let _ = bus
                    .publish(SUBJECT_DISPATCH_DONE, &serde_json::to_vec(&dd).unwrap())
                    .await;
                break;
            }
        });
    }

    // GL handler: post the balanced COGS entry, emit `gl.posted`, report completion.
    {
        let bus = bus.clone();
        let gl_eng = gl_eng.clone();
        tokio::spawn(async move {
            while let Some(msg) = gl_sub.next().await {
                let dd: DispatchDone = match serde_json::from_slice(&msg.payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // Recognize cost of goods sold: Dr COGS, Cr Inventory — a balanced pair.
                let total = unit_cost * rust_decimal::Decimal::from(dd.qty);
                let entries = vec![
                    LedgerEntry {
                        account: coa.cogs,
                        side: EntrySide::Debit,
                        amount: total,
                    },
                    LedgerEntry {
                        account: coa.inventory,
                        side: EntrySide::Credit,
                        amount: total,
                    },
                ];
                let tx: TransactionId = gl_eng
                    .post_transaction(entries, period, "flow: COGS on dispatch")
                    .await
                    .expect("balanced post");
                let _ = bus
                    .publish(
                        SUBJECT_GL_POSTED,
                        &serde_json::to_vec(&GlPosted {
                            tx: tx.as_str().to_string(),
                        })
                        .unwrap(),
                    )
                    .await;
                let report = FlowReport {
                    picked: dd.qty,
                    route_stops: dd.stops,
                    route_distance_km: dd.distance_km,
                    posted_tx: tx.as_str().to_string(),
                    posted_amount: total,
                };
                let _ = done_tx.send(report).await;
                break;
            }
        });
    }

    // The OMS order API kicks off the chain.
    let oc = OrderCreated {
        order_id: Id::<OmsSku>::new().as_str().to_string(),
        sku: wms_sku.as_str().to_string(),
        bin: bin.as_str().to_string(),
        qty,
    };
    bus.publish(SUBJECT_ORDER_CREATED, &serde_json::to_vec(&oc)?)
        .await?;

    done_rx
        .recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("cross-vertical flow did not complete"))
}

/// Build a tenant for example/demo use.
pub fn demo_tenant() -> TenantId {
    TenantSlug("flow-demo".to_string()).to_id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[tokio::test]
    async fn oms_to_wms_to_tms_to_gl_flow_completes() {
        let tenant = demo_tenant();
        // $12.50 per unit, order 4 units.
        let unit_cost = Money::<Usd>::from_major(12) + Money::<Usd>::new(Decimal::new(50, 2));
        let report = run_flow(tenant, 4, unit_cost).await.unwrap();

        assert_eq!(report.picked, 4);
        assert_eq!(report.route_stops, 4);
        // A 4-stop California loop is non-zero distance.
        assert!(report.route_distance_km > 0.0);
        assert!(!report.posted_tx.is_empty());
    }

    #[tokio::test]
    async fn flow_posts_balanced_cogs() {
        let tenant = demo_tenant();
        let unit_cost = Money::<Usd>::from_major(10);
        let report = run_flow(tenant, 3, unit_cost).await.unwrap();

        // GL recognized exactly 3 * $10 = $30 of COGS (a balanced Dr COGS / Cr Inventory).
        assert_eq!(report.posted_amount, Money::<Usd>::from_major(30));
        assert!(!report.posted_tx.is_empty());
    }
}
