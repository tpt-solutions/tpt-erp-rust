//! # pos — reference Retail / POS implementation on TPT ERP.
//!
//! A production-shaped point-of-sale engine built entirely on the framework's
//! primitives, with no shortcuts that would not survive real register load:
//!
//! - [`txn`] — a [`StateMachine`](tpt_erp_primitives::StateMachine)-derived sale
//!   lifecycle (`Cart -> Tendering -> Authorized -> Captured`, with `Voided`/`Refunded`
//!   branches), carrying [`Money<Usd>`](tpt_erp_primitives::Money) line items and tax.
//! - [`tender`] — `Money::allocate`-based **split tender** (multi-instrument payments
//!   that sum *exactly* to the total) and **cash-drawer reconciliation** (expected vs.
//!   counted variance).
//! - [`sync`] — **offline-first** selling: sales hit a local event-sourced log first,
//!   then replay idempotently to a central store on reconnect, publishing `pos.synced`
//!   and checkpointing via `tpt-erp-cache`.
//! - [`pricing`] — a real backend home for the `examples/plugins/pricing` Wasm guest:
//!   the store's loyalty balance drives the plugin's balance-tiered discount, hot-swappable
//!   at runtime.

pub mod pricing;
pub mod sync;
pub mod tender;
pub mod txn;

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    routing::post,
    Json, Router,
};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use tpt_erp_primitives::{Id, Money, Usd};
use tpt_erp_tenant::{TenantId, TenantSlug};

use crate::pricing::PosPricingEngine;
use crate::sync::{PosSync, SaleEvent};
use crate::tender::{Tender, TenderKind, split_total};
use crate::txn::{LineItem, TxnStatus, Transaction};

/// Build a tenant for example/demo use.
pub fn demo_tenant() -> TenantId {
    TenantSlug("pos-demo".to_string()).to_id()
}

/// A line in a sale request (price and tax in major units, e.g. dollars).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleLine {
    pub item: Id<crate::txn::PosItem>,
    pub name: String,
    pub qty: u32,
    pub unit_price: i64,
    pub tax: i64,
}

/// A tender in a sale request (amount in major units).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleTender {
    pub kind: TenderKind,
    pub amount: i64,
}

/// The result of ringing a sale at the register.
#[derive(Debug, Clone, Serialize)]
pub struct SaleOutcome {
    pub txn_id: String,
    pub subtotal: Money<Usd>,
    pub tax: Money<Usd>,
    pub discount: Money<Usd>,
    pub total: Money<Usd>,
    pub status: TxnStatus,
    pub applied_tenders: Vec<Money<Usd>>,
    pub pricing_applied: bool,
}

/// Errors surfaced by [`PosApp::sale`].
#[derive(Debug, thiserror::Error)]
pub enum PosError {
    #[error("insufficient tender: covered {covered}, total {total}")]
    Underfunded { covered: Money<Usd>, total: Money<Usd> },
    #[error("illegal transaction transition: {0}")]
    Transition(#[from] crate::txn::TxnStatusTransitionError),
    #[error("transaction error: {0}")]
    Txn(#[from] crate::txn::TxnError),
    #[error("transaction not editable")]
    NotEditable,
    #[error("sync failure: {0}")]
    Sync(#[from] crate::sync::SyncError),
}

/// The reference POS application bundle.
#[derive(Clone)]
pub struct PosApp {
    tenant: TenantId,
    terminal: Id<crate::sync::PosTerminal>,
    pub pricing: Arc<tokio::sync::Mutex<PosPricingEngine>>,
    pub sync: Arc<PosSync>,
}

impl PosApp {
    /// Build a demo POS for `tenant` with a fresh sync engine and no pricing plugin.
    pub fn new(tenant: TenantId) -> Self {
        let terminal = Id::new();
        Self {
            tenant,
            terminal,
            pricing: Arc::new(tokio::sync::Mutex::new(PosPricingEngine::without_plugin(
                tpt_erp_wasm::Money::new(0, 0),
                "pos",
            ))),
            sync: Arc::new(PosSync::new(tenant, terminal)),
        }
    }

    /// Load the `pricing` Wasm component to drive balance-tiered discounting.
    pub fn with_pricing(mut self, wasm: &[u8]) -> Result<Self, tpt_erp_wasm::RuntimeError> {
        let engine = PosPricingEngine::with_plugin(wasm, tpt_erp_wasm::Money::new(0, 0), "pos")?;
        self.pricing = Arc::new(tokio::sync::Mutex::new(engine));
        Ok(self)
    }

    /// Attach a background-job bus (for `pos.synced` lifecycle events).
    pub fn with_bus(mut self, bus: Box<dyn tpt_erp_bus::EventBus>) -> Self {
        self.sync = Arc::new(PosSync::new(self.tenant, self.terminal).with_bus(bus));
        self
    }

    /// Attach a read-model cache used as the sync checkpoint store.
    pub fn with_cache(mut self, cache: Box<dyn tpt_erp_cache::ReadModelCache>) -> Self {
        self.sync = Arc::new(PosSync::new(self.tenant, self.terminal).with_cache(cache));
        self
    }

    /// Ring a sale: build the transaction, apply the pricing discount, split tender
    /// exactly, advance the state machine to `Captured`, and record it offline.
    pub async fn sale(
        &self,
        lines: Vec<SaleLine>,
        tenders: Vec<SaleTender>,
    ) -> Result<SaleOutcome, PosError> {
        let mut txn = Transaction::new();
        for l in &lines {
            txn.add_line(LineItem {
                item: l.item,
                name: l.name.clone(),
                qty: l.qty,
                unit_price: Money::<Usd>::from_major(l.unit_price),
                tax: Money::<Usd>::from_major(l.tax),
            })?;
        }
        let subtotal = txn.subtotal();
        let tax = txn.tax_total();
        let gross = subtotal + tax;

        // Optional pricing discount on the gross, in cents.
        let gross_cents = (gross.amount() * rust_decimal::Decimal::from(100))
            .to_i64()
            .unwrap_or(0);
        let (discounted_cents, pricing_applied) = {
            let mut pricing = self.pricing.lock().await;
            match pricing.discount("store-1", gross_cents) {
                Some(c) if c < gross_cents => (c, true),
                _ => (gross_cents, false),
            }
        };
        let total = Money::<Usd>::new(rust_decimal::Decimal::from(discounted_cents)
            / rust_decimal::Decimal::from(100));
        let discount = gross - total;

        // Split the (possibly discounted) total across the offered tenders exactly.
        let offered: Vec<Tender> = tenders
            .iter()
            .map(|t| Tender {
                kind: t.kind,
                amount: Money::<Usd>::from_major(t.amount),
            })
            .collect();
        let applied = split_total(total, &offered).map_err(|e| match e {
            crate::tender::TenderError::Underfunded { covered, total } => {
                PosError::Underfunded { covered, total }
            }
            crate::tender::TenderError::EmptyTenders => {
                PosError::Underfunded { covered: Money::zero(), total: gross }
            }
        })?;

        // Advance the sale lifecycle to a captured (settled) state.
        txn.advance(TxnStatus::Tendering)?;
        txn.advance(TxnStatus::Authorized)?;
        txn.advance(TxnStatus::Captured)?;

        // Record the sale offline; it reconciles to central on the next sync.
        let sale = SaleEvent {
            txn_id: txn.id.as_str(),
            terminal: self.terminal.as_str(),
            total,
            tenders: offered
                .iter()
                .zip(applied.iter())
                .map(|(t, a)| (t.kind, *a))
                .collect(),
            at: chrono::Utc::now(),
        };
        self.sync.record_offline(sale)?;

        Ok(SaleOutcome {
            txn_id: txn.id.as_str(),
            subtotal,
            tax,
            discount,
            total,
            status: txn.status,
            applied_tenders: applied,
            pricing_applied,
        })
    }

    /// Build the Axum router: a `/sale` handler that rings a transaction.
    pub fn router(&self) -> Router {
        Router::new()
            .route(
                "/sale",
                post(sale_handler).put(sale_handler),
            )
            .with_state(PosState {
                app: self.clone(),
            })
    }
}

/// Axum state shared by the POS handlers.
#[derive(Clone)]
pub struct PosState {
    app: PosApp,
}

async fn sale_handler(
    State(st): State<PosState>,
    Json(req): Json<PosSaleRequest>,
) -> Result<Json<SaleOutcome>, (StatusCode, String)> {
    st.app
        .sale(req.lines, req.tenders)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// JSON body for the `/sale` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PosSaleRequest {
    pub lines: Vec<SaleLine>,
    pub tenders: Vec<SaleTender>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::txn::PosItem;

    fn line(item: Id<PosItem>, qty: u32, price: i64, tax: i64) -> SaleLine {
        SaleLine {
            item,
            name: "Item".into(),
            qty,
            unit_price: price,
            tax,
        }
    }

    #[tokio::test]
    async fn sale_rings_and_records_offline() {
        let app = PosApp::new(demo_tenant());
        let item = Id::new();
        let out = app
            .sale(
                vec![line(item, 2, 9, 1)],
                vec![SaleTender {
                    kind: TenderKind::Cash,
                    amount: 20,
                }],
            )
            .await
            .expect("sale ok");
        assert_eq!(out.subtotal, Money::<Usd>::from_major(18));
        assert_eq!(out.tax, Money::<Usd>::from_major(1));
        assert_eq!(out.total, Money::<Usd>::from_major(19));
        assert_eq!(out.status, TxnStatus::Captured);
        assert_eq!(out.applied_tenders.len(), 1);
        assert_eq!(app.sync.pending_count(), 1);
    }

    #[tokio::test]
    async fn underfunded_sale_is_rejected() {
        let app = PosApp::new(demo_tenant());
        let item = Id::new();
        let res = app
            .sale(
                vec![line(item, 1, 50, 0)],
                vec![SaleTender {
                    kind: TenderKind::Cash,
                    amount: 10,
                }],
            )
            .await;
        assert!(matches!(res, Err(PosError::Underfunded { .. })));
        assert_eq!(app.sync.pending_count(), 0);
    }
}
