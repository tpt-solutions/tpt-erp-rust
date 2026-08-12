//! Loyalty engine that *writes* balances.
//!
//! Unlike the read-only balance the pricing plugin consumes, this engine is the system
//! of record for a customer's loyalty position. It **accrues** points on each sale
//! (`points = f(subtotal)`) and **redeems** them at sale time, writing the balance down
//! and emitting a `StoreCredit` tender that offsets the amount due. Points and any
//! redeemable credit are kept in exact [`rust_decimal::Decimal`] / [`Money<Usd>`] — never
//! `f64`.

use rust_decimal::Decimal;
use std::collections::HashMap;
use parking_lot::Mutex;
use tpt_erp_primitives::{Id, Money, Usd};

use crate::txn::PosCustomer;

/// A customer's loyalty position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoyaltyBalance {
    /// Accrued points (exact decimal; 1 point == 1 major-unit of redeemed value).
    pub points: Decimal,
    /// Redeemable store credit (rarely used directly; redemptions burn `points`).
    pub credit: Money<Usd>,
}

impl Default for LoyaltyBalance {
    fn default() -> Self {
        Self {
            points: Decimal::ZERO,
            credit: Money::zero(),
        }
    }
}

/// Errors raised by the loyalty engine.
#[derive(Debug, thiserror::Error)]
pub enum LoyaltyError {
    #[error("insufficient loyalty balance: have {have}, want {want}")]
    Insufficient { have: Decimal, want: Money<Usd> },
}

/// Accrues and redeems per-customer loyalty points. In-memory persistence is sufficient
/// for the reference register; a production deployment would back this with a ledger.
pub struct LoyaltyEngine {
    balances: Mutex<HashMap<Id<PosCustomer>, LoyaltyBalance>>,
    /// Points earned per major unit of pre-tax subtotal.
    earn_rate: Decimal,
}

impl LoyaltyEngine {
    /// A new engine earning 1 point per major unit (e.g. dollar) of subtotal.
    pub fn new() -> Self {
        Self {
            balances: Mutex::new(HashMap::new()),
            earn_rate: Decimal::ONE,
        }
    }

    /// The current balance for `customer` (zero if unseen).
    pub fn balance(&self, customer: Id<PosCustomer>) -> LoyaltyBalance {
        self.balances
            .lock()
            .get(&customer)
            .cloned()
            .unwrap_or_default()
    }

    /// Accrue points for a sale: `points += subtotal * earn_rate`. Returns the new
    /// balance. This *writes* the balance — it persists across calls.
    pub fn earn(&self, customer: Id<PosCustomer>, subtotal: Money<Usd>) -> LoyaltyBalance {
        let mut map = self.balances.lock();
        let b = map.entry(customer).or_default();
        b.points += (subtotal.amount() * self.earn_rate).round_dp(4);
        b.clone()
    }

    /// Redeem `amount` of value by burning the equivalent points, writing the balance
    /// down. Errors if the customer has fewer points than `amount`. Returns the new
    /// balance.
    pub fn redeem(
        &self,
        customer: Id<PosCustomer>,
        amount: Money<Usd>,
    ) -> Result<LoyaltyBalance, LoyaltyError> {
        let mut map = self.balances.lock();
        let b = map.entry(customer).or_default();
        let have = b.points;
        if have < amount.amount() {
            return Err(LoyaltyError::Insufficient { have, want: amount });
        }
        b.points -= amount.amount();
        b.credit += amount;
        Ok(b.clone())
    }
}

impl Default for LoyaltyEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cust() -> Id<PosCustomer> {
        Id::new()
    }

    #[test]
    fn earns_points_on_sale_and_persists() {
        let eng = LoyaltyEngine::new();
        let c = cust();
        let after = eng.earn(c, Money::<Usd>::from_major(18));
        assert_eq!(after.points, Decimal::from(18));
        // Persisted: a second read shows the same balance.
        assert_eq!(eng.balance(c).points, Decimal::from(18));
    }

    #[test]
    fn redeem_writes_balance_down() {
        let eng = LoyaltyEngine::new();
        let c = cust();
        eng.earn(c, Money::<Usd>::from_major(18));
        let after = eng.redeem(c, Money::<Usd>::from_major(5)).unwrap();
        assert_eq!(after.points, Decimal::from(13));
        assert_eq!(after.credit, Money::<Usd>::from_major(5));
        assert_eq!(eng.balance(c).points, Decimal::from(13));
    }

    #[test]
    fn redeem_over_balance_is_rejected() {
        let eng = LoyaltyEngine::new();
        let c = cust();
        eng.earn(c, Money::<Usd>::from_major(3));
        assert!(eng.redeem(c, Money::<Usd>::from_major(5)).is_err());
    }
}
