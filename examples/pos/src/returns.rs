//! Returns and exchange flow.
//!
//! A `Captured` sale can be returned in full or line-level partial. The refund value is
//! derived from the returned merchandise so that **discount and tax stay correct**:
//! the customer paid `total` for `subtotal` of merchandise, so the refund for returned
//! merchandise `R` is the same fraction of `total` that `R` is of `subtotal` —
//! `refund = total * R / subtotal`. This preserves the pre-tax-discount / tax-on-
//! discounted-subtotal invariant of the sale.
//!
//! The refund is then split back across the **original tenders** in proportion to how
//! the customer paid, using the same largest-remainder [`Money::allocate`] math as
//! [`crate::tender::split_total`]. A full return therefore hands back exactly the
//! original tender amounts; a partial return apportions the smaller refund
//! proportionally and still sums exactly.
//!
//! An **exchange** is the same return plus a new sale in one transaction: the return
//! value becomes a `StoreCredit` tender applied to the new purchase, so the customer
//! only pays the net difference.

use rust_decimal::Decimal;
use tpt_erp_primitives::{Id, Money, Usd};

use crate::tender::{Tender, split_total};
use crate::txn::{LineItem, PosItem, TxnStatus};

/// A captured sale, retained by the app so it can later be returned or exchanged.
/// The full line and tender detail is needed to compute discount/tax-correct refunds.
/// Held in memory only (never serialized), so no serde derive.
#[derive(Debug, Clone)]
pub struct SaleRecord {
    /// Transaction id (idempotency key) of the original sale.
    pub id: String,
    /// Status the sale was in when recorded (expected `Captured`).
    pub status: TxnStatus,
    /// Pre-discount merchandise subtotal.
    pub subtotal: Money<Usd>,
    /// Discount applied to the pre-tax subtotal.
    pub discount: Money<Usd>,
    /// Tax charged on the discounted subtotal.
    pub tax: Money<Usd>,
    /// Grand total actually charged (discounted subtotal + tax).
    pub total: Money<Usd>,
    /// Tenders as applied at capture time (kinds + exact amounts, summing to `total`).
    pub tenders: Vec<Tender>,
    /// The line items that were sold.
    pub lines: Vec<LineItem>,
}

/// A request to return a quantity of a given item (by catalog id).
#[derive(Debug, Clone)]
pub struct ReturnLine {
    pub item: Id<PosItem>,
    pub qty: u32,
}

/// The result of a return: the exact refund amount and how it splays back across the
/// original tenders.
#[derive(Debug, Clone)]
pub struct RefundSplit {
    /// Total money refunded to the customer.
    pub refund_amount: Money<Usd>,
    /// Refund per original tender (kind + amount), summing exactly to `refund_amount`.
    pub tenders: Vec<Tender>,
}

/// Errors raised by the returns/exchange flow.
#[derive(Debug, thiserror::Error)]
pub enum ReturnError {
    #[error("sale {0} not found")]
    NotFound(String),
    #[error("sale is not returnable (status {0:?})")]
    NotReturnable(TxnStatus),
    #[error("cannot return more merchandise than was purchased")]
    OverReturn,
    #[error("refund tender split failed: {0}")]
    Tender(#[from] crate::tender::TenderError),
}

/// The merchandise value of the requested returns.
///
/// An empty `returns` list means a **full** return (all lines). Otherwise each
/// [`ReturnLine`] is matched against the original lines by item id and valued at the
/// original (pre-discount) unit price. Returning more value than was purchased is an
/// error.
pub fn returned_merchandise(
    record: &SaleRecord,
    returns: &[ReturnLine],
) -> Result<Money<Usd>, ReturnError> {
    let total = if returns.is_empty() {
        record.subtotal
    } else {
        let mut sum = Money::<Usd>::zero();
        for r in returns {
            let matched: Money<Usd> = record
                .lines
                .iter()
                .filter(|l| l.item == r.item)
                .map(|l| l.unit_price * Decimal::from(r.qty))
                .fold(Money::zero(), |a, b| a + b);
            sum = sum + matched;
        }
        sum
    };
    if total > record.subtotal {
        return Err(ReturnError::OverReturn);
    }
    Ok(total)
}

/// Compute the refund for returned merchandise `R`, preserving discount/tax correctness,
/// and split it back across the original tenders exactly.
///
/// The refund is `total * R / subtotal`, derived by largest-remainder allocation of
/// `total` into `[R, subtotal - R]` so the returned share is exact and sum-preserving.
/// The result is then re-apportioned across the original tenders proportionally to how
/// the customer paid (mirroring split-tender math), so a full return reproduces the
/// original tenders exactly.
pub fn compute_refund(
    record: &SaleRecord,
    returned: Money<Usd>,
) -> Result<RefundSplit, ReturnError> {
    if returned > record.subtotal {
        return Err(ReturnError::OverReturn);
    }
    let r_w = crate::tender::weight(returned);
    let s_w = crate::tender::weight(record.subtotal);
    // Largest-remainder split of `total` into [returned, remainder]; part 0 is the refund.
    let parts = record
        .total
        .allocate(&[r_w, s_w.saturating_sub(r_w)])
        .map_err(|_| ReturnError::Tender(crate::tender::TenderError::EmptyTenders))?;
    let refund_amount = parts[0];

    // Re-split the refund across the original tenders proportional to what was paid.
    let applied = split_total(refund_amount, &record.tenders)?;
    let tenders = record
        .tenders
        .iter()
        .zip(applied.iter())
        .map(|(t, a)| Tender {
            kind: t.kind,
            amount: *a,
        })
        .collect();

    Ok(RefundSplit {
        refund_amount,
        tenders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tender::TenderKind;
    use tpt_erp_primitives::Money;

    fn line(item: Id<PosItem>, qty: u32, price: i64, tax: i64) -> LineItem {
        LineItem {
            item,
            name: "Item".into(),
            qty,
            unit_price: Money::<Usd>::from_major(price),
            tax: Money::<Usd>::from_major(tax),
        }
    }

    fn record() -> SaleRecord {
        let a = Id::new();
        let b = Id::new();
        SaleRecord {
            id: "txn1".into(),
            status: TxnStatus::Captured,
            subtotal: Money::<Usd>::from_major(18),
            discount: Money::<Usd>::zero(),
            tax: Money::<Usd>::from_major(1),
            total: Money::<Usd>::from_major(19),
            tenders: vec![
                Tender {
                    kind: TenderKind::Cash,
                    amount: Money::<Usd>::from_major(10),
                },
                Tender {
                    kind: TenderKind::Card,
                    amount: Money::<Usd>::from_major(9),
                },
            ],
            lines: vec![line(a, 2, 9, 1), line(b, 1, 5, 0)],
        }
    }

    #[test]
    fn full_return_refunds_original_tenders_exactly() {
        let rec = record();
        let returned = returned_merchandise(&rec, &[]).unwrap();
        assert_eq!(returned, rec.subtotal);
        let split = compute_refund(&rec, returned).unwrap();
        assert_eq!(split.refund_amount, rec.total);
        let sum: Money<Usd> = split
            .tenders
            .iter()
            .map(|t| t.amount)
            .fold(Money::zero(), |a, b| a + b);
        assert_eq!(sum, rec.total);
        // Exact reproduction of the original split.
        assert_eq!(split.tenders[0].amount, Money::<Usd>::from_major(10));
        assert_eq!(split.tenders[1].amount, Money::<Usd>::from_major(9));
        assert_eq!(split.tenders[0].kind, TenderKind::Cash);
        assert_eq!(split.tenders[1].kind, TenderKind::Card);
    }

    #[test]
    fn partial_return_allocates_correctly() {
        let rec = record();
        // Return the single $5 item (pre-tax). Refund = 5/18 of the $19 total.
        let target = rec.lines[1].item;
        let returned = returned_merchandise(
            &rec,
            &[ReturnLine {
                item: target,
                qty: 1,
            }],
        )
        .unwrap();
        assert_eq!(returned, Money::<Usd>::from_major(5));
        let split = compute_refund(&rec, returned).unwrap();
        // 5/18 of 19 = 95/18 = 5.2777... allocated exactly to cents -> 5.28.
        assert_eq!(
            split.refund_amount,
            Money::<Usd>::new(Decimal::from(528) / Decimal::from(100))
        );
        let sum: Money<Usd> = split
            .tenders
            .iter()
            .map(|t| t.amount)
            .fold(Money::zero(), |a, b| a + b);
        assert_eq!(sum, split.refund_amount);
        // Proportional: cash share ~ 10/19 of refund, card ~ 9/19.
        assert!(split.tenders[0].amount > split.tenders[1].amount);
    }

    #[test]
    fn cannot_return_more_than_purchased() {
        let rec = record();
        let target = rec.lines[0].item;
        // Ask to return 99 of a line that only had 2.
        let res = returned_merchandise(
            &rec,
            &[ReturnLine {
                item: target,
                qty: 99,
            }],
        );
        assert!(matches!(res, Err(ReturnError::OverReturn)));
    }
}
