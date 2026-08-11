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

pub mod loyalty;
pub mod pricing;
pub mod returns;
pub mod sync;
pub mod tender;
pub mod txn;

use std::collections::HashMap;
use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use tpt_erp_primitives::{Id, Money, Usd};
use tpt_erp_tenant::{TenantId, TenantSlug};

use crate::loyalty::{LoyaltyEngine, LoyaltyError};
use crate::pricing::PosPricingEngine;
use crate::returns::{
    RefundSplit, ReturnError, ReturnLine, SaleRecord, compute_refund, returned_merchandise,
};
use crate::sync::{PosSync, SaleEvent, SaleKind};
use crate::tender::{Tender, TenderKind, split_total};
use crate::txn::{LineItem, PosCustomer, Transaction, TxnStatus};

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

/// The full result of ringing a sale (used by loyalty/exchange paths), including the
/// per-tender breakdown (kinds + amounts) and the loyalty points earned.
#[derive(Debug, Clone)]
pub struct RingOutcome {
    pub txn_id: String,
    pub subtotal: Money<Usd>,
    pub tax: Money<Usd>,
    pub discount: Money<Usd>,
    pub total: Money<Usd>,
    pub status: TxnStatus,
    /// Applied tenders with their instrument kinds (e.g. a `StoreCredit` redemption).
    pub applied_tenders: Vec<Tender>,
    pub pricing_applied: bool,
    /// Loyalty points accrued on this sale (exact decimal).
    pub earned_points: Decimal,
}

/// The result of an exchange: the return refund plus the new sale it was applied to.
#[derive(Debug, Clone)]
pub struct ExchangeOutcome {
    /// The return value credited toward the new sale.
    pub refund: Money<Usd>,
    /// The new sale, with the credit applied as a `StoreCredit` tender.
    pub sale: RingOutcome,
}

/// Errors surfaced by [`PosApp::sale`].
#[derive(Debug, thiserror::Error)]
pub enum PosError {
    #[error("insufficient tender: covered {covered}, total {total}")]
    Underfunded {
        covered: Money<Usd>,
        total: Money<Usd>,
    },
    #[error("illegal transaction transition: {0}")]
    Transition(#[from] crate::txn::TxnStatusTransitionError),
    #[error("transaction error: {0}")]
    Txn(#[from] crate::txn::TxnError),
    #[error("transaction not editable")]
    NotEditable,
    #[error("sale {0} not found")]
    NoSuchSale(String),
    #[error("return error: {0}")]
    Return(#[from] ReturnError),
    #[error("loyalty error: {0}")]
    Loyalty(#[from] LoyaltyError),
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
    /// Registry of completed sales, keyed by transaction id, so they can be returned
    /// or exchanged (carries full line + tender detail for discount/tax-correct refunds).
    pub sales: Arc<std::sync::Mutex<HashMap<String, SaleRecord>>>,
    /// Loyalty engine that accrues and redeems per-customer balances.
    pub loyalty: Arc<LoyaltyEngine>,
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
            sales: Arc::new(std::sync::Mutex::new(HashMap::new())),
            loyalty: Arc::new(LoyaltyEngine::new()),
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
    /// exactly, advance the state machine to `Captured`, and record it offline. No
    /// loyalty customer is attached.
    pub async fn sale(
        &self,
        lines: Vec<SaleLine>,
        tenders: Vec<SaleTender>,
    ) -> Result<SaleOutcome, PosError> {
        let r = self
            .ring_sale(lines, tenders, None, Money::zero(), Money::zero())
            .await?;
        Ok(SaleOutcome {
            txn_id: r.txn_id,
            subtotal: r.subtotal,
            tax: r.tax,
            discount: r.discount,
            total: r.total,
            status: r.status,
            applied_tenders: r.applied_tenders.iter().map(|t| t.amount).collect(),
            pricing_applied: r.pricing_applied,
        })
    }

    /// Ring a sale for `customer`, accruing loyalty points and (optionally) redeeming
    /// `redeem` of earned balance as a `StoreCredit` tender that offsets the total.
    pub async fn sale_for(
        &self,
        customer: Id<PosCustomer>,
        redeem: Money<Usd>,
        lines: Vec<SaleLine>,
        tenders: Vec<SaleTender>,
    ) -> Result<RingOutcome, PosError> {
        self.ring_sale(lines, tenders, Some(customer), redeem, Money::zero())
            .await
    }

    /// Core sale engine shared by [`PosApp::sale`], [`PosApp::sale_for`], and
    /// [`PosApp::exchange`].
    ///
    /// Discount applies to the pre-tax subtotal; tax is computed on the discounted
    /// subtotal (the existing correctness invariant). `loyalty_redeem` burns the
    /// customer's earned points (writing the balance down) and is applied as a
    /// `StoreCredit` tender. `exchange_credit` is a return value applied as `StoreCredit`
    /// without touching loyalty. Both credits are applied *exactly* (capped at the total)
    /// — they are not proportionally diluted by an oversupplied cash tender — then points
    /// are accrued on the subtotal and persisted.
    async fn ring_sale(
        &self,
        lines: Vec<SaleLine>,
        tenders: Vec<SaleTender>,
        customer: Option<Id<PosCustomer>>,
        loyalty_redeem: Money<Usd>,
        exchange_credit: Money<Usd>,
    ) -> Result<RingOutcome, PosError> {
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

        // Feed the customer's earned loyalty balance to the pricing host so the
        // balance-tiered discount reflects what they have actually accrued.
        if let Some(c) = customer {
            let pts = self.loyalty.balance(c).points;
            let wasm = points_to_wasm_money(pts);
            self.pricing.lock().await.set_balance(wasm).await;
        }

        // Pricing discount on the *pre-tax subtotal* (not the tax-inclusive gross), so the
        // effective tax rate is unchanged. `pricing.discount` returns the discounted
        // subtotal in cents.
        let subtotal_cents = (subtotal.amount() * Decimal::from(100))
            .to_i64()
            .unwrap_or(0);
        let (discounted_subtotal_cents, pricing_applied) = {
            let mut pricing = self.pricing.lock().await;
            match pricing.discount("store-1", subtotal_cents) {
                Some(c) if c < subtotal_cents => (c, true),
                _ => (subtotal_cents, false),
            }
        };
        let discounted_subtotal =
            Money::<Usd>::new(Decimal::from(discounted_subtotal_cents) / Decimal::from(100));
        let discount = subtotal - discounted_subtotal;
        let total = discounted_subtotal + tax;

        // Validate the loyalty redemption against the earned balance, then combine the
        // credits. The store-credit tender is applied *exactly* (capped at the total) so
        // the customer's points/credit are consumed as intended.
        if loyalty_redeem > Money::zero() {
            let c = customer.ok_or_else(|| {
                PosError::Loyalty(LoyaltyError::Insufficient {
                    have: Decimal::ZERO,
                    want: loyalty_redeem,
                })
            })?;
            let have = self.loyalty.balance(c).points;
            if have < loyalty_redeem.amount() {
                return Err(PosError::Loyalty(LoyaltyError::Insufficient {
                    have,
                    want: loyalty_redeem,
                }));
            }
        }
        let credit_total = loyalty_redeem + exchange_credit;
        let applied_credit = credit_total.min(total);
        let applied_loyalty = loyalty_redeem.min(total);

        // The remaining amount is covered by the explicitly tendered instruments.
        let remaining = total - applied_credit;
        let offered: Vec<Tender> = tenders
            .iter()
            .map(|t| Tender {
                kind: t.kind,
                amount: Money::<Usd>::from_major(t.amount),
            })
            .collect();
        let applied_remaining = if remaining > Money::zero() {
            split_total(remaining, &offered).map_err(|e| match e {
                crate::tender::TenderError::Underfunded { covered, total } => {
                    PosError::Underfunded { covered, total }
                }
                crate::tender::TenderError::EmptyTenders => PosError::Underfunded {
                    covered: Money::zero(),
                    total: subtotal,
                },
            })?
        } else {
            vec![Money::zero(); offered.len()]
        };

        let mut applied_tenders: Vec<Tender> = Vec::with_capacity(offered.len() + 1);
        if applied_credit > Money::zero() {
            applied_tenders.push(Tender {
                kind: TenderKind::StoreCredit,
                amount: applied_credit,
            });
        }
        applied_tenders.extend(
            offered
                .iter()
                .zip(applied_remaining.iter())
                .map(|(t, a)| Tender {
                    kind: t.kind,
                    amount: *a,
                }),
        );

        // Advance the sale lifecycle to a captured (settled) state.
        txn.advance(TxnStatus::Tendering)?;
        txn.advance(TxnStatus::Authorized)?;
        txn.advance(TxnStatus::Captured)?;

        // Persist the sale so it can later be returned or exchanged.
        self.sales.lock().unwrap().insert(
            txn.id.as_str(),
            SaleRecord {
                id: txn.id.as_str(),
                status: txn.status,
                subtotal,
                discount,
                tax,
                total,
                tenders: applied_tenders.clone(),
                lines: txn.lines.clone(),
            },
        );

        // Accrue loyalty points on the subtotal, then write down the redeemed balance.
        let mut earned_points = Decimal::ZERO;
        if let Some(c) = customer {
            let b = self.loyalty.earn(c, subtotal);
            earned_points = b.points;
            if applied_loyalty > Money::zero() {
                let b = self.loyalty.redeem(c, applied_loyalty)?;
                earned_points = b.points;
            }
        }

        // Record the sale offline; it reconciles to central on the next sync.
        self.sync.record_offline(SaleEvent {
            txn_id: txn.id.as_str(),
            terminal: self.terminal.as_str(),
            kind: SaleKind::Sale,
            total,
            tenders: applied_tenders.iter().map(|t| (t.kind, t.amount)).collect(),
            at: chrono::Utc::now(),
        })?;

        Ok(RingOutcome {
            txn_id: txn.id.as_str(),
            subtotal,
            tax,
            discount,
            total,
            status: txn.status,
            applied_tenders,
            pricing_applied,
            earned_points,
        })
    }

    /// Return a captured sale in full (`returns` empty) or line-level partial. Produces a
    /// discount/tax-correct refund that splits back across the original tenders, and emits
    /// the return through the offline sync log like a normal sale.
    pub async fn return_sale(
        &self,
        txn_id: &str,
        returns: Vec<ReturnLine>,
    ) -> Result<RefundSplit, PosError> {
        let record = self
            .sales
            .lock()
            .unwrap()
            .get(txn_id)
            .cloned()
            .ok_or_else(|| PosError::NoSuchSale(txn_id.to_string()))?;
        if record.status != TxnStatus::Captured {
            return Err(PosError::Return(ReturnError::NotReturnable(record.status)));
        }
        // Demonstrate the legal return transition through the state machine.
        let mut t = Transaction::new();
        t.advance(TxnStatus::Tendering)?;
        t.advance(TxnStatus::Authorized)?;
        t.advance(TxnStatus::Captured)?;
        t.advance(TxnStatus::Returned)?;

        let returned = returned_merchandise(&record, &returns)?;
        let split = compute_refund(&record, returned)?;

        self.sync.record_offline(SaleEvent {
            txn_id: format!("ret-{}", Id::<crate::txn::PosTxn>::new().as_str()),
            terminal: self.terminal.as_str(),
            kind: SaleKind::Return,
            total: split.refund_amount,
            tenders: split.tenders.iter().map(|t| (t.kind, t.amount)).collect(),
            at: chrono::Utc::now(),
        })?;
        Ok(split)
    }

    /// Exchange: return `original_txn_id` and apply the return value toward `new_lines`
    /// in one transaction. The return value becomes a `StoreCredit` tender on the new
    /// sale; the customer pays only the net difference. Both the return and the new sale
    /// are emitted through the offline sync log.
    pub async fn exchange(
        &self,
        original_txn_id: &str,
        returns: Vec<ReturnLine>,
        new_lines: Vec<SaleLine>,
        new_tenders: Vec<SaleTender>,
        customer: Option<Id<PosCustomer>>,
    ) -> Result<ExchangeOutcome, PosError> {
        let record = self
            .sales
            .lock()
            .unwrap()
            .get(original_txn_id)
            .cloned()
            .ok_or_else(|| PosError::NoSuchSale(original_txn_id.to_string()))?;
        if record.status != TxnStatus::Captured {
            return Err(PosError::Return(ReturnError::NotReturnable(record.status)));
        }
        let mut t = Transaction::new();
        t.advance(TxnStatus::Tendering)?;
        t.advance(TxnStatus::Authorized)?;
        t.advance(TxnStatus::Captured)?;
        t.advance(TxnStatus::Returned)?;

        let returned = returned_merchandise(&record, &returns)?;
        let refund = compute_refund(&record, returned)?;

        // Emit the return event (offline-first, like a normal sale).
        self.sync.record_offline(SaleEvent {
            txn_id: format!("ret-{}", Id::<crate::txn::PosTxn>::new().as_str()),
            terminal: self.terminal.as_str(),
            kind: SaleKind::Return,
            total: refund.refund_amount,
            tenders: refund
                .tenders
                .iter()
                .map(|tt| (tt.kind, tt.amount))
                .collect(),
            at: chrono::Utc::now(),
        })?;

        // Apply the return value toward the new purchase as `StoreCredit`.
        let sale = self
            .ring_sale(
                new_lines,
                new_tenders,
                customer,
                Money::zero(),
                refund.refund_amount,
            )
            .await?;
        Ok(ExchangeOutcome {
            refund: refund.refund_amount,
            sale,
        })
    }

    /// Build the Axum router: a `/sale` handler that rings a transaction.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/sale", post(sale_handler).put(sale_handler))
            .with_state(PosState { app: self.clone() })
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

/// Convert an exact loyalty-point decimal into the WIT-portable `wasm` [`Money`] the
/// pricing plugin reads (major units + 1/10_000ths minor), with no floating point.
fn points_to_wasm_money(points: Decimal) -> tpt_erp_wasm::Money {
    let major = points.trunc().to_i64().unwrap_or(0);
    let minor = ((points - points.trunc()) * Decimal::from(10_000))
        .trunc()
        .to_i64()
        .unwrap_or(0);
    tpt_erp_wasm::Money::new(major, minor)
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

    #[tokio::test]
    async fn full_return_refunds_original_tenders_exactly() {
        let app = PosApp::new(demo_tenant());
        let a = Id::new();
        // Subtotal $18, tax $1, total $19; tenders exactly cover it ($10 cash, $9 card).
        let out = app
            .sale(
                vec![line(a, 2, 9, 1)],
                vec![
                    SaleTender {
                        kind: TenderKind::Cash,
                        amount: 10,
                    },
                    SaleTender {
                        kind: TenderKind::Card,
                        amount: 9,
                    },
                ],
            )
            .await
            .expect("sale ok");
        assert_eq!(out.total, Money::<Usd>::from_major(19));
        assert_eq!(app.sync.pending_count(), 1);

        let split = app
            .return_sale(&out.txn_id, vec![])
            .await
            .expect("return ok");
        // Full return hands back exactly the original tenders ($10 cash, $9 card).
        assert_eq!(split.refund_amount, Money::<Usd>::from_major(19));
        let cash = split
            .tenders
            .iter()
            .find(|t| t.kind == TenderKind::Cash)
            .unwrap();
        let card = split
            .tenders
            .iter()
            .find(|t| t.kind == TenderKind::Card)
            .unwrap();
        assert_eq!(cash.amount, Money::<Usd>::from_major(10));
        assert_eq!(card.amount, Money::<Usd>::from_major(9));
        // The return is emitted as its own offline event.
        assert_eq!(app.sync.pending_count(), 2);
    }

    #[tokio::test]
    async fn partial_return_allocates_correctly() {
        let app = PosApp::new(demo_tenant());
        let a = Id::new();
        let b = Id::new();
        // Two $10 items, no tax; total $20 across $10 cash + $10 card.
        let out = app
            .sale(
                vec![line(a, 1, 10, 0), line(b, 1, 10, 0)],
                vec![
                    SaleTender {
                        kind: TenderKind::Cash,
                        amount: 10,
                    },
                    SaleTender {
                        kind: TenderKind::Card,
                        amount: 10,
                    },
                ],
            )
            .await
            .expect("sale ok");
        assert_eq!(out.total, Money::<Usd>::from_major(20));

        // Return only item b ($10 of $20) => refund is exactly $10, split 50/50.
        let split = app
            .return_sale(&out.txn_id, vec![ReturnLine { item: b, qty: 1 }])
            .await
            .expect("return ok");
        assert_eq!(split.refund_amount, Money::<Usd>::from_major(10));
        let cash = split
            .tenders
            .iter()
            .find(|t| t.kind == TenderKind::Cash)
            .unwrap();
        let card = split
            .tenders
            .iter()
            .find(|t| t.kind == TenderKind::Card)
            .unwrap();
        assert_eq!(cash.amount, Money::<Usd>::from_major(5));
        assert_eq!(card.amount, Money::<Usd>::from_major(5));
        let sum: Money<Usd> = split
            .tenders
            .iter()
            .map(|t| t.amount)
            .fold(Money::zero(), |x, y| x + y);
        assert_eq!(sum, split.refund_amount);
        assert_eq!(app.sync.pending_count(), 2);
    }

    #[tokio::test]
    async fn exchange_applies_return_value_to_new_sale() {
        let app = PosApp::new(demo_tenant());
        let a = Id::new();
        // Original sale: subtotal $18, tax $1, total $19 across $10 cash + $9 card.
        let original = app
            .sale(
                vec![line(a, 2, 9, 1)],
                vec![
                    SaleTender {
                        kind: TenderKind::Cash,
                        amount: 10,
                    },
                    SaleTender {
                        kind: TenderKind::Card,
                        amount: 9,
                    },
                ],
            )
            .await
            .expect("sale ok");

        // Exchange: return the whole original ($19) and buy a $30 item, paying $11 cash.
        let c = Id::new();
        let ex = app
            .exchange(
                &original.txn_id,
                vec![],
                vec![line(c, 1, 30, 0)],
                vec![SaleTender {
                    kind: TenderKind::Cash,
                    amount: 11,
                }],
                None,
            )
            .await
            .expect("exchange ok");

        // New sale total is $30; the $19 return credit plus $11 cash cover it exactly.
        assert_eq!(ex.sale.total, Money::<Usd>::from_major(30));
        let credit = ex
            .sale
            .applied_tenders
            .iter()
            .find(|t| t.kind == TenderKind::StoreCredit)
            .unwrap();
        assert_eq!(credit.amount, Money::<Usd>::from_major(19));
        let cash = ex
            .sale
            .applied_tenders
            .iter()
            .find(|t| t.kind == TenderKind::Cash)
            .unwrap();
        assert_eq!(cash.amount, Money::<Usd>::from_major(11));
        // Return event + new sale event are both in the offline log.
        assert_eq!(app.sync.pending_count(), 3);
    }

    #[tokio::test]
    async fn illegal_return_transitions_rejected() {
        // A Cart transaction cannot jump straight to Returned.
        let mut t = Transaction::new();
        assert!(t.advance(TxnStatus::Returned).is_err());

        // Returning an unknown sale id is rejected.
        let app = PosApp::new(demo_tenant());
        let res = app.return_sale("does-not-exist", vec![]).await;
        assert!(matches!(res, Err(PosError::NoSuchSale(_))));
    }

    #[tokio::test]
    async fn loyalty_earns_and_redeems() {
        let app = PosApp::new(demo_tenant());
        let cust = Id::new();
        let item = Id::new();

        // Sale earns points equal to the pre-tax subtotal ($18 -> 18 points).
        let first = app
            .sale_for(
                cust,
                Money::<Usd>::zero(),
                vec![line(item, 2, 9, 1)],
                vec![SaleTender {
                    kind: TenderKind::Cash,
                    amount: 20,
                }],
            )
            .await
            .expect("sale ok");
        assert_eq!(first.earned_points, Decimal::from(18));
        assert_eq!(app.loyalty.balance(cust).points, Decimal::from(18));

        // A second sale redeems $5 of the earned balance as StoreCredit, writing it down.
        let item2 = Id::new();
        let second = app
            .sale_for(
                cust,
                Money::<Usd>::from_major(5),
                vec![line(item2, 1, 10, 0)],
                vec![SaleTender {
                    kind: TenderKind::Cash,
                    amount: 20,
                }],
            )
            .await
            .expect("sale ok");
        // 18 earned, +10 earned on this sale, -5 redeemed => 23 points.
        assert_eq!(app.loyalty.balance(cust).points, Decimal::from(23));
        let credit = second
            .applied_tenders
            .iter()
            .find(|t| t.kind == TenderKind::StoreCredit)
            .unwrap();
        assert_eq!(credit.amount, Money::<Usd>::from_major(5));
    }
}
