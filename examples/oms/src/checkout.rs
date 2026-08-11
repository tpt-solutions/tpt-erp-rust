//! Axum wiring + Wasm promo integration + the end-to-end checkout.
//!
//! [`OmsApp`] bundles the reservation engine, the GL journal (shared with
//! [`crate::saga`]), and an optional promo Wasm plugin into one object with a
//! single [`OmsApp::checkout`] entry point. [`OmsApp::router`] mounts the
//! `TptEntity`/`TptApi` catalog routers (role-differentiated RBAC) alongside a
//! `/checkout` handler that runs the order saga and applies the promo discount.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use tpt_erp_bus::{BusError, EventBus, MessageStream};
use tpt_erp_entity::InMemoryRepository;
use tpt_erp_primitives::{Id, Money, Usd};
use tpt_erp_tenant::TenantId;

use crate::catalog::{
    CustomerAuth, OrderApi, OrderRow, OrderStatus, ProductApi, ProductRow, StaffAuth,
};
use crate::promo::PromoEngine;
use crate::reservation::ReservationEngine;
use crate::saga::{OrderSaga, SagaError, SagaLine};

/// A single checkout line (price already known; catalog lookup is out of scope for the
/// reference flow).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutLine {
    pub sku: Id<crate::reservation::Sku>,
    pub qty: u32,
    pub unit_price: Money<Usd>,
}

/// The result of a checkout: pre/post-discount economics plus the saga outcome.
#[derive(Debug, Clone, Serialize)]
pub struct CheckoutOutcome {
    pub status: OrderStatus,
    pub transaction_id: Option<String>,
    pub subtotal: Money<Usd>,
    pub discount: Money<Usd>,
    pub total: Money<Usd>,
    pub promo_applied: bool,
    /// Lines that could not be reserved and were placed on backorder (awaiting stock).
    pub backorders: Vec<SagaLine>,
}

/// Errors surfaced by [`OmsApp::checkout`].
#[derive(Debug, thiserror::Error)]
pub enum OmsError {
    #[error("saga failed: {0}")]
    Saga(#[from] SagaError),
}

/// A boxed `EventBus` carried behind an `Arc` so it can be cloned into route state.
struct ArcBus(Arc<dyn EventBus>);

#[async_trait]
impl EventBus for ArcBus {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), BusError> {
        self.0.publish(subject, payload).await
    }
    async fn subscribe(&self, subject: &str) -> Result<MessageStream, BusError> {
        self.0.subscribe(subject).await
    }
}

/// The reference OMS application bundle.
#[derive(Clone)]
pub struct OmsApp {
    tenant: TenantId,
    pub reservation: Arc<ReservationEngine>,
    journal: Arc<gl::journal::JournalEngine<Usd>>,
    coa: gl::coa::DemoAccounts<Usd>,
    bus: Option<Arc<dyn EventBus>>,
    pub promo: Arc<tokio::sync::Mutex<PromoEngine>>,
}

impl OmsApp {
    /// Build a demo OMS for `tenant` with an empty reservation engine, a fresh GL
    /// journal, and no promo plugin loaded.
    pub fn new(tenant: TenantId) -> Self {
        let (journal, coa) = gl::journal::demo(tenant);
        let reservation = Arc::new(ReservationEngine::new(tenant));
        Self {
            tenant,
            reservation: reservation.clone(),
            journal: Arc::new(journal),
            coa,
            bus: None,
            promo: Arc::new(tokio::sync::Mutex::new(PromoEngine::without_plugin(
                reservation,
                "oms",
            ))),
        }
    }

    /// Attach a background-job bus (for saga/catalog lifecycle events).
    pub fn with_bus(mut self, bus: Arc<dyn EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Load a compiled `promo` Wasm component to drive stock-aware discounting.
    pub fn with_promo(mut self, wasm: &[u8]) -> Result<Self, tpt_erp_wasm::RuntimeError> {
        let engine = PromoEngine::with_plugin(wasm, self.reservation.clone(), "oms")?;
        self.promo = Arc::new(tokio::sync::Mutex::new(engine));
        Ok(self)
    }

    /// Run a checkout: reserve stock, apply the promo discount, post the sale through
    /// the GL journal, fulfill, and ship.
    pub async fn checkout(&self, lines: Vec<CheckoutLine>) -> Result<CheckoutOutcome, OmsError> {
        let mut promo = self.promo.lock().await;
        let mut saga_lines: Vec<SagaLine> = Vec::with_capacity(lines.len());
        let mut subtotal = Money::<Usd>::zero();
        let mut total = Money::<Usd>::zero();
        let mut promo_applied = false;

        for l in &lines {
            let unit_cents = (l.unit_price.amount() * Decimal::from(100))
                .to_i64()
                .unwrap_or(0);
            let final_cents = match promo.discount(&l.sku.as_str(), l.qty as u64, unit_cents) {
                Some(c) if c < unit_cents => {
                    promo_applied = true;
                    c
                }
                _ => unit_cents,
            };
            let final_price = Money::<Usd>::new(Decimal::from(final_cents) / Decimal::from(100));

            subtotal += l.unit_price * Decimal::from(l.qty);
            total += final_price * Decimal::from(l.qty);
            saga_lines.push(SagaLine {
                sku: l.sku,
                qty: l.qty,
                unit_price: final_price,
            });
        }
        drop(promo);

        let discount = subtotal - total;

        let mut saga = OrderSaga::new(
            self.tenant,
            self.reservation.clone(),
            self.journal.clone(),
            self.coa.clone(),
        );
        if let Some(bus) = &self.bus {
            saga = saga.with_bus(Box::new(ArcBus(bus.clone())));
        }

        let outcome = saga.run(&saga_lines).await?;
        Ok(CheckoutOutcome {
            status: outcome.status,
            transaction_id: outcome.transaction_id.map(|t| t.as_str()),
            subtotal,
            discount,
            total,
            promo_applied,
            backorders: outcome.backorders,
        })
    }

    /// Fulfill a backordered checkout once stock has arrived. Delegates to the order saga,
    /// which reserves, bills, and commits the backordered lines; the order then ships.
    pub async fn fulfill_backorders(
        &self,
        backorders: &[SagaLine],
    ) -> Result<CheckoutOutcome, OmsError> {
        let saga = OrderSaga::new(
            self.tenant,
            self.reservation.clone(),
            self.journal.clone(),
            self.coa.clone(),
        );
        let outcome = saga.fulfill_backorders(backorders).await?;
        Ok(CheckoutOutcome {
            status: outcome.status,
            transaction_id: outcome.transaction_id.map(|t| t.as_str()),
            subtotal: Money::<Usd>::zero(),
            discount: Money::<Usd>::zero(),
            total: outcome.total,
            promo_applied: false,
            backorders: outcome.backorders,
        })
    }

    /// Build the Axum router: catalog CRUD (role-differentiated RBAC) + `/checkout`.
    pub fn router(&self) -> Router {
        let products = Arc::new(InMemoryRepository::<ProductRow>::new());
        let orders = Arc::new(InMemoryRepository::<OrderRow>::new());

        let catalog = Router::new()
            .merge(ProductApi::router::<_, StaffAuth>(products))
            .merge(OrderApi::router::<_, CustomerAuth>(orders));

        let checkout_router = Router::new()
            .route("/checkout", post(checkout_handler))
            .with_state(OmsState { app: self.clone() });

        Router::new().merge(catalog).merge(checkout_router)
    }
}

/// Axum state shared by the `/checkout` handler.
#[derive(Clone)]
pub struct OmsState {
    app: OmsApp,
}

async fn checkout_handler(
    State(st): State<OmsState>,
    Json(req): Json<Vec<CheckoutLine>>,
) -> Result<Json<CheckoutOutcome>, (StatusCode, String)> {
    st.app
        .checkout(req)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_primitives::Money;

    fn line(sku: Id<crate::reservation::Sku>, qty: u32, price: i64) -> CheckoutLine {
        CheckoutLine {
            sku,
            qty,
            unit_price: Money::<Usd>::from_major(price),
        }
    }

    #[tokio::test]
    async fn checkout_reserves_pays_and_ships() {
        let tenant = crate::reservation::demo_tenant();
        let app = OmsApp::new(tenant);
        let sku = Id::new();
        app.reservation.receive(sku, 10).await.unwrap();

        let out = app
            .checkout(vec![line(sku, 2, 10)])
            .await
            .expect("checkout ok");
        assert_eq!(out.status, OrderStatus::Shipped);
        assert_eq!(out.total, Money::<Usd>::from_major(20));
        assert!(out.transaction_id.is_some());
        // Stock: 10 received, 2 committed (fulfilled), 8 still sellable.
        assert_eq!(app.reservation.available(sku), 8);
        assert_eq!(app.reservation.on_hand(sku), 10);
    }

    #[tokio::test]
    async fn checkout_backorders_when_stock_short_then_fulfills() {
        let tenant = crate::reservation::demo_tenant();
        let app = OmsApp::new(tenant);
        let sku = Id::new();
        // Only 2 in stock, but the customer wants 5.
        app.reservation.receive(sku, 2).await.unwrap();

        let out = app.checkout(vec![line(sku, 5, 10)]).await.unwrap();
        // The 2 available units are reserved/committed; the remaining 3 are backordered.
        // `total` is the full order total (all 5 units); the billed amount for the
        // in-stock portion is reflected once stock arrives and the backorder is fulfilled.
        assert_eq!(out.status, OrderStatus::Backordered);
        assert_eq!(out.total, Money::<Usd>::from_major(50));
        assert_eq!(out.backorders.len(), 1);
        assert_eq!(out.backorders[0].qty, 3);
        // 2 committed, nothing else sellable yet.
        assert_eq!(app.reservation.available(sku), 0);

        // Stock arrives for the backorder.
        app.reservation.receive(sku, 3).await.unwrap();
        let out2 = app.fulfill_backorders(&out.backorders).await.unwrap();
        assert_eq!(out2.status, OrderStatus::Shipped);
        assert_eq!(out2.total, Money::<Usd>::from_major(30));
        // All 5 units committed; nothing sellable.
        assert_eq!(app.reservation.available(sku), 0);
        assert_eq!(app.reservation.on_hand(sku), 5);
    }

    // Concurrent checkouts against limited stock must never oversell end-to-end.
    #[tokio::test]
    #[ignore = "concurrent stress test; run explicitly"]
    async fn concurrent_checkout_no_oversell() {
        let tenant = crate::reservation::demo_tenant();
        let app = OmsApp::new(tenant);
        let sku = Id::new();
        let stock = 50i64;
        app.reservation.receive(sku, stock).await.unwrap();
        let app = Arc::new(app);

        let tasks: Vec<_> = (0..200)
            .map(|_| {
                let app = app.clone();
                tokio::spawn(async move { app.checkout(vec![line(sku, 1, 10)]).await.is_ok() })
            })
            .collect();
        let mut ok = 0;
        for t in tasks {
            if t.await.unwrap() {
                ok += 1;
            }
        }
        // Exactly the available stock was sold; no unit was allocated twice.
        assert_eq!(ok, stock as usize);
        assert_eq!(app.reservation.available(sku), 0);
    }
}
