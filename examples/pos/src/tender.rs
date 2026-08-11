//! Split tender and cash-drawer reconciliation.
//!
//! A sale may be paid with **multiple tenders** (e.g. `$20` cash plus the remainder on
//! a card). Splitting the grand total across tenders must be *exact* at the currency's
//! minor unit — no penny lost or invented. We use [`Money::allocate`] (largest-remainder
//! apportionment) so the parts always sum back to the original [`Money`].
//!
//! Reconciliation compares the **expected** cash (the sum of every `Cash` tender the
//! register recorded) against the **counted** cash a manager physically counted at
//! shift end. The variance is the discrepancy that needs explaining.

use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use tpt_erp_primitives::{Currency, Money, Usd};

/// The kind of payment instrument used for a tender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TenderKind {
    Cash,
    Card,
    GiftCard,
    StoreCredit,
}

impl TenderKind {
    /// Whether this tender kind is physical cash that must be present in the drawer.
    pub fn is_cash(&self) -> bool {
        matches!(self, TenderKind::Cash)
    }
}

/// A single applied tender: an instrument kind and the amount tendered.
#[derive(Debug, Clone)]
pub struct Tender {
    pub kind: TenderKind,
    pub amount: Money<Usd>,
}

/// Errors raised by split-tender math.
#[derive(Debug, thiserror::Error)]
pub enum TenderError {
    #[error("cannot split an empty set of tenders")]
    EmptyTenders,
    #[error("tender split does not cover the total: covered {covered}, total {total}")]
    Underfunded {
        covered: Money<Usd>,
        total: Money<Usd>,
    },
}

/// Split `total` across `tenders` so that each tender's *applied* share is apportioned
/// proportionally to the raw amount it offered, and the shares sum *exactly* to `total`
/// at the minor unit.
///
/// Tenders that offer more than the total still only receive their proportional share;
/// if the tenders jointly cover less than `total` (sum of offered amounts < `total`) then
/// [`TenderError::Underfunded`] is returned — the caller knows the sale is not fully paid.
pub fn split_total(total: Money<Usd>, tenders: &[Tender]) -> Result<Vec<Money<Usd>>, TenderError> {
    if tenders.is_empty() {
        return Err(TenderError::EmptyTenders);
    }
    // The customer must have tendered at least the sale total across all instruments.
    let offered: Money<Usd> = tenders
        .iter()
        .map(|t| t.amount)
        .fold(Money::zero(), |a, b| a + b);
    if offered < total {
        return Err(TenderError::Underfunded {
            covered: offered,
            total,
        });
    }
    // Split the (fully covered) total proportionally to each tender's offered amount; the
    // parts always sum *exactly* back to `total` at the minor unit. `allocate` returns an
    // error only for an empty/zero-weight list, which cannot happen here (weights are
    // derived from non-empty `tenders` and cannot all be zero since `offered >= total > 0`).
    let weights: Vec<u64> = tenders.iter().map(|t| amount_to_u64(t.amount)).collect();
    let applied = total
        .allocate(&weights)
        .map_err(|_| TenderError::EmptyTenders)?;
    Ok(applied)
}

/// A physical cash drawer: the cash the register *expected* versus what was *counted*.
#[derive(Debug, Clone)]
pub struct Drawer {
    pub expected_cash: Money<Usd>,
    pub counted_cash: Money<Usd>,
}

impl Drawer {
    /// Build a drawer from an expected vs. counted count (both in [`Usd`]).
    pub fn new(expected_cash: Money<Usd>, counted_cash: Money<Usd>) -> Self {
        Self {
            expected_cash,
            counted_cash,
        }
    }

    /// The discrepancy: positive means over (more counted than expected), negative
    /// means short.
    pub fn variance(&self) -> Money<Usd> {
        self.counted_cash - self.expected_cash
    }

    /// Whether the drawer is in balance (zero variance).
    pub fn is_balanced(&self) -> bool {
        self.variance() == Money::<Usd>::zero()
    }
}

/// Extract the minor-unit integer of a [`Money`] as a `u64` weight for [`Money::allocate`].
fn amount_to_u64<C: Currency>(m: Money<C>) -> u64 {
    // Scale to minor units (e.g. cents) so the proportional split is meaningful; guard
    // against negative amounts by saturating at zero.
    let minor = (m.amount() * Decimal::from(10).powi(C::MINOR_UNITS as i64))
        .to_i64()
        .unwrap_or(0);
    minor.max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn tender(kind: TenderKind, major: i64) -> Tender {
        Tender {
            kind,
            amount: Money::<Usd>::from_major(major),
        }
    }

    #[test]
    fn split_sums_exactly_to_total() {
        let total = Money::<Usd>::from_major(19);
        let tenders = vec![
            tender(TenderKind::Cash, 10),
            tender(TenderKind::Card, 9),
            tender(TenderKind::GiftCard, 5),
        ];
        let applied = split_total(total, &tenders).unwrap();
        let sum = applied.iter().cloned().fold(Money::zero(), |a, b| a + b);
        assert_eq!(sum, total);
        // Cash got the largest single share (highest weight).
        assert!(applied[0] >= applied[1]);
    }

    #[test]
    fn split_never_invents_or_loses_a_cent() {
        // An awkward total that does not divide evenly across three tenders. The tenders
        // jointly cover the total ($12 offered against a $10.03 sale), so the split must
        // still sum *exactly* back to $10.03 at the minor unit.
        let total = Money::<Usd>::new(Decimal::from(1003) / Decimal::from(100)); // $10.03
        let tenders = vec![
            tender(TenderKind::Cash, 5),
            tender(TenderKind::Card, 4),
            tender(TenderKind::StoreCredit, 3),
        ];
        let applied = split_total(total, &tenders).unwrap();
        let sum = applied.iter().cloned().fold(Money::zero(), |a, b| a + b);
        assert_eq!(sum, total);
    }

    #[test]
    fn underfunded_split_is_reported() {
        let total = Money::<Usd>::from_major(50);
        let tenders = vec![tender(TenderKind::Cash, 20)];
        assert!(matches!(
            split_total(total, &tenders),
            Err(TenderError::Underfunded { .. })
        ));
    }

    #[test]
    fn drawer_variance_and_balance() {
        let balanced = Drawer::new(Money::<Usd>::from_major(200), Money::<Usd>::from_major(200));
        assert!(balanced.is_balanced());
        assert_eq!(balanced.variance(), Money::<Usd>::zero());

        let short = Drawer::new(Money::<Usd>::from_major(200), Money::<Usd>::from_major(195));
        assert!(!short.is_balanced());
        assert_eq!(short.variance(), Money::<Usd>::from_major(-5));

        let over = Drawer::new(Money::<Usd>::from_major(200), Money::<Usd>::from_major(203));
        assert_eq!(over.variance(), Money::<Usd>::from_major(3));
    }

    #[test]
    fn expected_cash_only_counts_cash_tenders() {
        let tenders = vec![
            tender(TenderKind::Cash, 50),
            tender(TenderKind::Card, 30),
            tender(TenderKind::GiftCard, 20),
        ];
        let expected: Money<Usd> = tenders
            .iter()
            .filter(|t| t.kind.is_cash())
            .map(|t| t.amount)
            .fold(Money::zero(), |a, b| a + b);
        assert_eq!(expected, Money::<Usd>::from_major(50));
    }
}
