//! Returns / RMA lifecycle for the OMS reference implementation.
//!
//! A return references an original [`Order`](crate::catalog::Order) and the specific
//! line items / quantities the customer is sending back. It is modeled as its own
//! event-sourced RMA record with a [`StateMachine`]-enforced status, decoupled from the
//! order so a *partial* return only reverses the returned stock and refunds the returned
//! amount — the rest of the order stays fulfilled.
//!
//! The RMA lifecycle is `Requested -> Authorized -> Received -> Refunded`, with a
//! `Rejected` branch. On `Received` the returned units are restored to the sellable
//! stock pool (reversing the committed allocation); on `Refunded` a balanced reversing
//! GL transaction (`Cr AccountsReceivable, Dr SalesRevenue`) is posted, mirroring the
//! `Pay` step in [`crate::saga`].

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tpt_erp_bus::EventBus;
use tpt_erp_ledger::{EntrySide, LedgerEntry, TransactionId};
use tpt_erp_primitives::{Entity, Id, Money, StateMachine, Usd};

use crate::catalog::Order;
use crate::reservation::{ReservationEngine, ReservationError, Sku};
use gl::coa::DemoAccounts;
use gl::journal::{JournalEngine, JournalError};

/// A return merchandise authorization (RMA) aggregate.
#[derive(Debug)]
pub struct Rma;
impl Entity for Rma {}

/// One line of a return: the SKU and quantity being sent back, priced at the
/// original order's unit price so the refund is exact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RmaLine {
    pub sku: Id<Sku>,
    pub qty: u32,
    pub unit_price: Money<Usd>,
}

/// The lifecycle of an RMA, enforced by a [`StateMachine`].
///
/// `Requested -> Authorized -> Received -> Refunded`, with a `Rejected` branch from
/// either `Requested` or `Received`. Illegal jumps (e.g. `Requested -> Received`) are
/// rejected at runtime with a typed error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, StateMachine)]
#[state_machine(transitions(
    Requested => Authorized,
    Requested => Rejected,
    Authorized => Received,
    Authorized => Rejected,
    Received => Refunded,
    Received => Rejected,
))]
pub enum RmaStatus {
    Requested,
    Authorized,
    Received,
    Refunded,
    Rejected,
}

/// A return merchandise authorization record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RmaRow {
    pub id: Id<Rma>,
    pub order: Id<Order>,
    pub status: RmaStatus,
    pub lines: Vec<RmaLine>,
    /// The exact refund amount: sum of `unit_price * qty` over the returned lines.
    pub refund_total: Money<Usd>,
}

/// Errors raised while processing a return.
#[derive(Debug, thiserror::Error)]
pub enum ReturnError {
    #[error("invalid RMA state transition: {0}")]
    Transition(#[from] RmaStatusTransitionError),
    #[error("returned stock could not be restored: {0}")]
    Reservation(#[from] ReservationError),
    #[error("refund posting failed: {0}")]
    Ledger(#[from] JournalError),
}

/// Processes returns: restocks returned units and posts the refund transaction.
pub struct RmaProcessor {
    reservation: std::sync::Arc<ReservationEngine>,
    journal: std::sync::Arc<JournalEngine<Usd>>,
    coa: DemoAccounts<Usd>,
    bus: Option<Box<dyn EventBus>>,
    period: String,
}

impl RmaProcessor {
    /// Build a return processor over the reservation engine and GL journal.
    pub fn new(
        reservation: std::sync::Arc<ReservationEngine>,
        journal: std::sync::Arc<JournalEngine<Usd>>,
        coa: DemoAccounts<Usd>,
    ) -> Self {
        Self {
            reservation,
            journal,
            coa,
            bus: None,
            period: "2026-01".to_string(),
        }
    }

    /// Attach a bus for RMA lifecycle events.
    pub fn with_bus(mut self, bus: Box<dyn EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Override the accounting period used when posting the refund.
    pub fn with_period(mut self, period: impl Into<String>) -> Self {
        self.period = period.into();
        self
    }

    /// Open a new RMA in the `Requested` state, referencing `order` and the lines being
    /// returned. The refund total is computed from the returned quantities.
    pub fn request(&self, order: Id<Order>, lines: Vec<RmaLine>) -> RmaRow {
        let refund_total = lines.iter().fold(Money::<Usd>::zero(), |acc, l| {
            acc + l.unit_price * Decimal::from(l.qty)
        });
        RmaRow {
            id: Id::new(),
            order,
            status: RmaStatus::Requested,
            lines,
            refund_total,
        }
    }

    /// `Requested -> Authorized`. The return has been approved but stock has not yet
    /// physically come back.
    pub fn authorize(&self, rma: &mut RmaRow) -> Result<(), ReturnError> {
        rma.status = rma.status.transition(RmaStatus::Authorized)?;
        self.publish(rma, "oms.rma.authorized");
        Ok(())
    }

    /// `Authorized -> Received`. The returned units are restored to the sellable pool,
    /// reversing the committed stock allocation (event-sourced via `Return`).
    pub async fn receive(&self, rma: &mut RmaRow) -> Result<(), ReturnError> {
        rma.status = rma.status.transition(RmaStatus::Received)?;
        for line in &rma.lines {
            self.reservation
                .return_stock(line.sku, line.qty as i64)
                .await?;
        }
        self.publish(rma, "oms.rma.received");
        Ok(())
    }

    /// `Received -> Refunded`. Posts the balanced reversing GL transaction for the
    /// returned amount.
    pub async fn refund(&self, rma: &mut RmaRow) -> Result<TransactionId, ReturnError> {
        rma.status = rma.status.transition(RmaStatus::Refunded)?;
        let total = rma.refund_total;
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
        let tx = self
            .journal
            .post_transaction(entries, &self.period, "oms refund")
            .await?;
        self.publish(rma, "oms.rma.refunded");
        Ok(tx)
    }

    /// Run the full RMA in one call: authorize, receive (restock), and refund (post GL).
    /// A partial return only restocks and refunds the returned lines; the order's
    /// remaining fulfilled lines are untouched.
    pub async fn complete(&self, rma: &mut RmaRow) -> Result<TransactionId, ReturnError> {
        self.authorize(rma)?;
        self.receive(rma).await?;
        self.refund(rma).await
    }

    fn publish(&self, rma: &RmaRow, subject: &str) {
        if let Some(bus) = &self.bus {
            let _ = bus.publish(
                subject,
                serde_json::json!({ "rma": rma.id.as_str(), "order": rma.order.as_str(), "refund": rma.refund_total.amount().to_string() })
                    .to_string()
                    .as_bytes(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reservation::demo_tenant;

    fn line(sku: Id<Sku>, qty: u32, price: i64) -> RmaLine {
        RmaLine {
            sku,
            qty,
            unit_price: Money::<Usd>::from_major(price),
        }
    }

    #[test]
    fn illegal_rma_transitions_rejected() {
        // Requested cannot jump straight to Received or Refunded.
        assert!(!RmaStatus::Requested.can_transition(RmaStatus::Received));
        assert!(!RmaStatus::Requested.can_transition(RmaStatus::Refunded));
        // Received can only go to Refunded (or Rejected).
        assert!(!RmaStatus::Received.can_transition(RmaStatus::Authorized));
        // Valid paths are accepted.
        assert!(RmaStatus::Requested.can_transition(RmaStatus::Authorized));
        assert!(RmaStatus::Received.can_transition(RmaStatus::Refunded));
    }

    #[tokio::test]
    async fn return_restocks_and_posts_refund() {
        let tenant = demo_tenant();
        let (journal, coa) = gl::journal::demo(tenant);
        let reservation = std::sync::Arc::new(ReservationEngine::new(tenant));
        let sku = Id::new();
        reservation.receive(sku, 10).await.unwrap();
        // Customer bought 4 and ships them (committed).
        let hold = reservation
            .reserve(sku, 4, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        reservation.confirm(sku, hold).await.unwrap();
        assert_eq!(reservation.available(sku), 6);

        let proc = RmaProcessor::new(reservation.clone(), std::sync::Arc::new(journal), coa);
        let order = Id::new();
        let mut rma = proc.request(order, vec![line(sku, 4, 10)]);
        let tx = proc.complete(&mut rma).await.unwrap();

        // RMA is fully refunded and the 4 units are back in the sellable pool.
        assert_eq!(rma.status, RmaStatus::Refunded);
        assert!(!tx.as_str().is_empty());
        assert_eq!(rma.refund_total, Money::<Usd>::from_major(40));
        assert_eq!(reservation.available(sku), 10);
    }

    #[tokio::test]
    async fn partial_return_keeps_rest_fulfilled() {
        let tenant = demo_tenant();
        let (journal, coa) = gl::journal::demo(tenant);
        let reservation = std::sync::Arc::new(ReservationEngine::new(tenant));
        let sku = Id::new();
        reservation.receive(sku, 10).await.unwrap();
        let hold = reservation
            .reserve(sku, 5, std::time::Duration::from_secs(60))
            .await
            .unwrap();
        reservation.confirm(sku, hold).await.unwrap();
        assert_eq!(reservation.available(sku), 5);

        let proc = RmaProcessor::new(reservation.clone(), std::sync::Arc::new(journal), coa);
        let order = Id::new();
        // Only 3 of the 5 shipped units are returned.
        let mut rma = proc.request(order, vec![line(sku, 3, 10)]);
        proc.complete(&mut rma).await.unwrap();

        // Refund is only for the returned 3; 2 units remain committed (fulfilled).
        assert_eq!(rma.refund_total, Money::<Usd>::from_major(30));
        assert_eq!(reservation.available(sku), 8); // 5 committed -> 2 committed + 3 returned
    }

    #[tokio::test]
    async fn rejected_return_does_not_restock_or_refund() {
        let tenant = demo_tenant();
        let (journal, coa) = gl::journal::demo(tenant);
        let reservation = std::sync::Arc::new(ReservationEngine::new(tenant));
        let sku = Id::new();
        reservation.receive(sku, 10).await.unwrap();

        let proc = RmaProcessor::new(reservation.clone(), std::sync::Arc::new(journal), coa);
        let mut rma = proc.request(Id::new(), vec![line(sku, 2, 10)]);
        proc.authorize(&mut rma).unwrap();
        rma.status = rma.status.transition(RmaStatus::Rejected).unwrap();
        assert_eq!(rma.status, RmaStatus::Rejected);
        // No stock moved, no refund posted.
        assert_eq!(reservation.available(sku), 10);
    }
}
