//! Order saga: a hand-rolled compensating-transaction orchestrator.
//!
//! The saga drives an order through `Reserve -> Pay -> Fulfill -> Ship`. Each step
//! publishes a progress event on `tpt-erp-bus`; if any step fails the orchestrator
//! runs the *compensating* actions for every step already completed, in reverse, and
//! publishes an `oms.order.compensated` event. The `Pay` step posts a real, balanced
//! double-entry transaction through [`gl::journal`], so revenue is booked atomically
//! with the rest of the workflow.

use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tpt_erp_bus::EventBus;
use tpt_erp_ledger::{EntrySide, LedgerEntry, TransactionId};
use tpt_erp_primitives::{Id, Money, Usd};
use tpt_erp_tenant::TenantId;

use crate::reservation::{Hold, ReservationEngine, ReservationError, Sku};
use gl::coa::DemoAccounts;
use gl::journal::{JournalEngine, JournalError};

/// One line the saga needs to reserve and bill.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SagaLine {
    pub sku: Id<Sku>,
    pub qty: u32,
    pub unit_price: Money<Usd>,
}

/// How far the saga progressed before failure (drives compensation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SagaStage {
    Reserved,
    Paid,
    Fulfilled,
}

/// The terminal result of a saga run.
#[derive(Debug)]
pub struct SagaOutcome {
    pub status: crate::catalog::OrderStatus,
    pub transaction_id: Option<TransactionId>,
    pub total: Money<Usd>,
    /// Lines that could not be reserved and were placed on backorder (awaiting stock).
    /// Empty when the order was fully fulfilled in one pass.
    pub backorders: Vec<SagaLine>,
}

/// Errors raised by the saga orchestrator.
#[derive(Debug, thiserror::Error)]
pub enum SagaError {
    #[error("reservation failed: {0}")]
    Reserve(#[from] ReservationError),
    #[error("payment posting failed: {0}")]
    Pay(#[from] JournalError),
    #[error("fulfillment failed: {0}")]
    Fulfill(ReservationError),
    /// The saga failed and fully compensated; `stage` is how far it got.
    #[error("order saga compensated at stage {0:?}")]
    Compensated(SagaStage),
}

/// Order saga orchestrator.
pub struct OrderSaga {
    tenant: TenantId,
    reservation: Arc<ReservationEngine>,
    journal: Arc<JournalEngine<Usd>>,
    coa: DemoAccounts<Usd>,
    bus: Option<Box<dyn EventBus>>,
    hold_ttl: Duration,
    period: String,
}

impl OrderSaga {
    /// Build a saga over the given reservation engine and GL journal.
    pub fn new(
        tenant: TenantId,
        reservation: Arc<ReservationEngine>,
        journal: Arc<JournalEngine<Usd>>,
        coa: DemoAccounts<Usd>,
    ) -> Self {
        Self {
            tenant,
            reservation,
            journal,
            coa,
            bus: None,
            hold_ttl: Duration::from_secs(15 * 60), // 15-minute cart hold
            period: "2026-01".to_string(),
        }
    }

    /// Attach a bus for saga-progress / compensation events.
    pub fn with_bus(mut self, bus: Box<dyn EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Override the accounting period used when posting the sale.
    pub fn with_period(mut self, period: impl Into<String>) -> Self {
        self.period = period.into();
        self
    }

    /// Run the `Reserve -> Pay -> Fulfill -> Ship` workflow. When stock is short the
    /// unreservable quantity is placed on backorder rather than failing: the available
    /// portion is reserved, billed, and committed, and the order parks at `Backordered`.
    /// Call [`OrderSaga::fulfill_backorders`] once stock arrives to finish the order.
    pub async fn run(&self, lines: &[SagaLine]) -> Result<SagaOutcome, SagaError> {
        if lines.is_empty() {
            return Err(SagaError::Reserve(ReservationError::InsufficientStock {
                sku: Id::<Sku>::new(),
                available: 0,
                requested: 1,
            }));
        }

        // --- Step 1: Reserve what is available; backorder the shortfall -----
        let mut holds: Vec<(Id<Sku>, Id<Hold>)> = Vec::with_capacity(lines.len());
        let mut backorders: Vec<SagaLine> = Vec::new();
        let mut total = Money::<Usd>::zero();
        for line in lines {
            let (reserved, backorder) = self
                .reservation
                .reserve_or_backorder(line.sku, line.qty, self.hold_ttl)
                .await?;
            if let Some((hold, qty)) = reserved {
                holds.push((line.sku, hold));
                total += line.unit_price * Decimal::from(qty);
            }
            if let Some((_, qty)) = backorder {
                backorders.push(SagaLine {
                    sku: line.sku,
                    qty,
                    unit_price: line.unit_price,
                });
            }
        }
        self.publish(
            "oms.order.reserved",
            serde_json::json!({ "lines": holds.len(), "backorders": backorders.len() }),
        )
        .await;

        // If nothing could be reserved, there is nothing to bill yet — the whole order
        // is on backorder.
        if holds.is_empty() {
            return Ok(SagaOutcome {
                status: crate::catalog::OrderStatus::Backordered,
                transaction_id: None,
                total: Money::<Usd>::zero(),
                backorders,
            });
        }

        // --- Step 2: Pay (post a balanced GL transaction) -------------------
        let tx_id = match self.post_sale(total).await {
            Ok(id) => Some(id),
            Err(e) => {
                self.compensate(SagaStage::Reserved, &holds, None, total)
                    .await;
                return Err(SagaError::Pay(e));
            }
        };
        self.publish(
            "oms.order.paid",
            serde_json::json!({ "tx": tx_id.map(|t| t.as_str()) }),
        )
        .await;

        // --- Step 3: Fulfill (commit the holds) -----------------------------
        for (sku, hold) in &holds {
            if let Err(e) = self.reservation.confirm(*sku, *hold).await {
                self.compensate(SagaStage::Paid, &holds, tx_id, total).await;
                return Err(SagaError::Fulfill(e));
            }
        }
        self.publish("oms.order.fulfilled", serde_json::json!({}))
            .await;

        // --- Step 4: Ship, or park on backorder if any line is still short --
        let status = if backorders.is_empty() {
            self.publish("oms.order.shipped", serde_json::json!({}))
                .await;
            crate::catalog::OrderStatus::Shipped
        } else {
            self.publish(
                "oms.order.backordered",
                serde_json::json!({ "lines": backorders.len() }),
            )
            .await;
            crate::catalog::OrderStatus::Backordered
        };

        Ok(SagaOutcome {
            status,
            transaction_id: tx_id,
            total,
            backorders,
        })
    }

    /// Finish a backordered order once stock has arrived. Promotes the backordered
    /// quantities (now fulfillable) into holds via the reservation engine, bills them
    /// with a balanced GL sale, commits, and ships. The returned [`SagaOutcome`] reflects
    /// the fulfilled backorders only (callers add it to the original run's total).
    pub async fn fulfill_backorders(
        &self,
        backorders: &[SagaLine],
    ) -> Result<SagaOutcome, SagaError> {
        if backorders.is_empty() {
            return Err(SagaError::Reserve(ReservationError::InsufficientStock {
                sku: Id::<Sku>::new(),
                available: 0,
                requested: 0,
            }));
        }

        // Price per SKU, taken from the order's own backordered lines.
        let price_for: std::collections::HashMap<Id<Sku>, Money<Usd>> =
            backorders.iter().map(|l| (l.sku, l.unit_price)).collect();

        // Promote every fulfillable backorder into a real hold (event-sourced).
        let filled = self.reservation.fulfill_backorders().await?;
        if filled.is_empty() {
            return Err(SagaError::Reserve(ReservationError::InsufficientStock {
                sku: Id::<Sku>::new(),
                available: 0,
                requested: 1,
            }));
        }

        let mut holds: Vec<(Id<Sku>, Id<Hold>)> = Vec::with_capacity(filled.len());
        let mut total = Money::<Usd>::zero();
        for (sku, hold, qty) in filled {
            holds.push((sku, hold));
            let unit = price_for
                .get(&sku)
                .copied()
                .unwrap_or_else(Money::<Usd>::zero);
            total += unit * Decimal::from(qty);
        }

        let tx_id = match self.post_sale(total).await {
            Ok(id) => Some(id),
            Err(e) => {
                for (sku, hold) in &holds {
                    let _ = self.reservation.release(*sku, *hold).await;
                }
                return Err(SagaError::Pay(e));
            }
        };

        for (sku, hold) in &holds {
            if let Err(e) = self.reservation.confirm(*sku, *hold).await {
                self.compensate(SagaStage::Paid, &holds, tx_id, total).await;
                return Err(SagaError::Fulfill(e));
            }
        }

        self.publish("oms.order.shipped", serde_json::json!({}))
            .await;

        Ok(SagaOutcome {
            status: crate::catalog::OrderStatus::Shipped,
            transaction_id: tx_id,
            total,
            backorders: Vec::new(),
        })
    }

    /// Post the sale: `Dr AccountsReceivable, Cr SalesRevenue`.
    async fn post_sale(&self, total: Money<Usd>) -> Result<TransactionId, JournalError> {
        let entries = vec![
            LedgerEntry {
                account: self.coa.accounts_receivable,
                side: EntrySide::Debit,
                amount: total,
            },
            LedgerEntry {
                account: self.coa.sales_revenue,
                side: EntrySide::Credit,
                amount: total,
            },
        ];
        self.journal
            .post_transaction(entries, &self.period, "oms sale")
            .await
    }

    /// Reverse a posted sale: `Cr AccountsReceivable, Dr SalesRevenue`.
    async fn refund(&self, total: Money<Usd>) {
        let entries = vec![
            LedgerEntry {
                account: self.coa.accounts_receivable,
                side: EntrySide::Credit,
                amount: total,
            },
            LedgerEntry {
                account: self.coa.sales_revenue,
                side: EntrySide::Debit,
                amount: total,
            },
        ];
        // Best-effort: a real system would route this to a补偿 job queue.
        let _ = self
            .journal
            .post_transaction(entries, &self.period, "oms refund")
            .await;
    }

    /// Run the compensating transactions for every completed step, in reverse.
    async fn compensate(
        &self,
        stage: SagaStage,
        holds: &[(Id<Sku>, Id<Hold>)],
        tx_id: Option<TransactionId>,
        total: Money<Usd>,
    ) {
        match stage {
            SagaStage::Paid | SagaStage::Fulfilled => {
                // Undo the GL posting first, then release the stock holds. Refund the
                // order's own total (not the account's cumulative balance) so we only
                // reverse what this saga posted.
                if tx_id.is_some() {
                    self.refund(total).await;
                }
                for (sku, hold) in holds {
                    let _ = self.reservation.release(*sku, *hold).await;
                }
            }
            SagaStage::Reserved => {
                for (sku, hold) in holds {
                    let _ = self.reservation.release(*sku, *hold).await;
                }
            }
        }
        self.publish(
            "oms.order.compensated",
            serde_json::json!({ "at_stage": format!("{:?}", stage) }),
        )
        .await;
    }

    async fn publish(&self, subject: &str, payload: serde_json::Value) {
        if let Some(bus) = &self.bus {
            // Await the publish so the lifecycle event is actually delivered; dropping the
            // future here (as before) silently lost every `oms.order.*` event.
            let _ = bus.publish(subject, payload.to_string().as_bytes()).await;
        }
    }

    #[allow(unused)]
    fn _tenant(&self) -> &TenantId {
        &self.tenant
    }
}
