//! Point-of-sale transaction lifecycle.
//!
//! A sale is modeled as a [`StateMachine`]-derived [`TxnStatus`] so an illegal jump
//! (e.g. `Cart -> Captured` skipping authorization, or `Voided -> Authorized`) is a
//! typed runtime error rather than a silent state corruption. The legal graph is the
//! single source of truth:
//!
//! ```text
//! Cart ─▶ Tendering ─▶ Authorized ─▶ Captured
//!          │              │              │
//!          ▼              ▼              ▼
//!        Voided        Voided        Refunded
//! ```
//!
//! A transaction carries [`LineItem`]s whose prices and tax are [`Money<Usd>`], so a
//! currency mix-up is impossible by construction; the totals are derived by exact
//! decimal arithmetic.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tpt_erp_primitives::{Entity, Id, Money, StateMachine, Usd};

/// Marker entity for a POS transaction.
#[derive(Debug)]
pub struct PosTxn;
impl Entity for PosTxn {}

/// Marker entity for a catalog item sold at the register.
#[derive(Debug)]
pub struct PosItem;
impl Entity for PosItem {}

/// A single line on a transaction: a catalog item, a quantity, an exact unit price,
/// and the tax charged on that line — all in [`Usd`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineItem {
    pub item: Id<PosItem>,
    pub name: String,
    pub qty: u32,
    pub unit_price: Money<Usd>,
    pub tax: Money<Usd>,
}

impl LineItem {
    /// The extended (qty × unit) merchandise value for the line.
    pub fn merchandise(&self) -> Money<Usd> {
        self.unit_price * Decimal::from(self.qty)
    }

    /// The merchandise value plus the line tax.
    pub fn total(&self) -> Money<Usd> {
        self.merchandise() + self.tax
    }
}

/// The lifecycle of a POS transaction.
///
/// `Cart -> Tendering -> Authorized -> Captured`, with a pre-capture `Voided`
/// branch (from `Tendering` or `Authorized`) and a post-capture `Refunded` branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, StateMachine)]
#[state_machine(transitions(
    Cart => Tendering,
    Tendering => Authorized,
    Authorized => Captured,
    Tendering => Voided,
    Authorized => Voided,
    Captured => Refunded,
    Captured => Voided,
))]
pub enum TxnStatus {
    Cart,
    Tendering,
    Authorized,
    Captured,
    Voided,
    Refunded,
}

/// A register transaction: its status, line items, and derived money totals.
#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: Id<PosTxn>,
    pub status: TxnStatus,
    pub lines: Vec<LineItem>,
}

impl Transaction {
    /// A fresh transaction in the `Cart` state.
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            status: TxnStatus::Cart,
            lines: Vec::new(),
        }
    }

    /// Append a line item while still editable (`Cart` only).
    pub fn add_line(&mut self, line: LineItem) -> Result<(), TxnError> {
        if self.status != TxnStatus::Cart {
            return Err(TxnError::NotEditable(self.status));
        }
        self.lines.push(line);
        Ok(())
    }

    /// Merchandise subtotal (qty × unit) across all lines.
    pub fn subtotal(&self) -> Money<Usd> {
        self.lines
            .iter()
            .map(|l| l.merchandise())
            .fold(Money::<Usd>::zero(), |a, b| a + b)
    }

    /// Total tax across all lines.
    pub fn tax_total(&self) -> Money<Usd> {
        self.lines
            .iter()
            .map(|l| l.tax)
            .fold(Money::<Usd>::zero(), |a, b| a + b)
    }

    /// Grand total: subtotal + tax.
    pub fn total(&self) -> Money<Usd> {
        self.subtotal() + self.tax_total()
    }

    /// Attempt a state transition, enforcing the prerequisite graph.
    pub fn advance(&mut self, to: TxnStatus) -> Result<(), TxnError> {
        self.status = self.status.transition(to)?;
        Ok(())
    }

    /// Whether the transaction can still be edited.
    pub fn is_editable(&self) -> bool {
        matches!(self.status, TxnStatus::Cart)
    }

    /// Whether the transaction reached a terminal sale state.
    pub fn is_settled(&self) -> bool {
        matches!(
            self.status,
            TxnStatus::Captured | TxnStatus::Refunded | TxnStatus::Voided
        )
    }
}

impl Default for Transaction {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors raised by the POS transaction engine.
#[derive(Debug, thiserror::Error)]
pub enum TxnError {
    #[error("illegal transaction transition: {0}")]
    Transition(#[from] TxnStatusTransitionError),
    #[error("transaction in {0:?} is not editable")]
    NotEditable(TxnStatus),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_enforces_sale_graph() {
        let mut t = Transaction::new();
        assert_eq!(t.status, TxnStatus::Cart);

        t.advance(TxnStatus::Tendering).unwrap();
        t.advance(TxnStatus::Authorized).unwrap();
        t.advance(TxnStatus::Captured).unwrap();

        // Cannot jump Captured -> Authorized (no reopen of a captured sale).
        assert!(t.advance(TxnStatus::Authorized).is_err());
        // Captured can be refunded.
        assert!(t.advance(TxnStatus::Refunded).is_ok());
    }

    #[test]
    fn pre_capture_void_branch() {
        let mut t = Transaction::new();
        t.advance(TxnStatus::Tendering).unwrap();
        t.advance(TxnStatus::Voided).unwrap();
        assert!(t.is_settled());
        // Voided is terminal: nothing follows.
        assert!(t.advance(TxnStatus::Captured).is_err());
    }

    #[test]
    fn totals_use_exact_decimal_money() {
        let mut t = Transaction::new();
        let item = Id::new();
        t.add_line(LineItem {
            item,
            name: "Mug".into(),
            qty: 2,
            unit_price: Money::<Usd>::from_major(9),
            tax: Money::<Usd>::from_major(1),
        })
        .unwrap();
        assert_eq!(t.subtotal(), Money::<Usd>::from_major(18));
        assert_eq!(t.tax_total(), Money::<Usd>::from_major(1));
        assert_eq!(t.total(), Money::<Usd>::from_major(19));
    }

    #[test]
    fn cannot_edit_after_tendering() {
        let mut t = Transaction::new();
        t.advance(TxnStatus::Tendering).unwrap();
        let item = Id::new();
        let res = t.add_line(LineItem {
            item,
            name: "Mug".into(),
            qty: 1,
            unit_price: Money::<Usd>::from_major(9),
            tax: Money::<Usd>::zero(),
        });
        assert!(matches!(res, Err(TxnError::NotEditable(_))));
    }
}
