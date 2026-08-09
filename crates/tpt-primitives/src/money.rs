//! Type-safe money.
//!
//! [`Money`] wraps a [`rust_decimal::Decimal`] tagged with a [`Currency`] marker `C`.
//! Because the currency is part of the *type*, `Money<Usd>` and `Money<Eur>` are
//! different types and cannot be accidentally added, subtracted, or compared.

use crate::currency::Currency;
use core::hash::Hash;
use core::ops::{Add, AddAssign, Neg, Sub, SubAssign};
use rust_decimal::Decimal;
use rust_decimal::MathematicalOps;
use rust_decimal::RoundingStrategy;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A precise amount of money in a specific currency `C`.
///
/// ```compile_fail
/// use tpt_primitives::{Money, Usd, Eur};
/// let a = Money::<Usd>::from_major(10);
/// let b = Money::<Eur>::from_major(5);
/// let _ = a + b; // currency mismatch — does not compile
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Money<C: Currency> {
    amount: Decimal,
    _currency: core::marker::PhantomData<C>,
}

impl<C: Currency> PartialEq for Money<C> {
    fn eq(&self, other: &Self) -> bool {
        self.amount == other.amount
    }
}

impl<C: Currency> Eq for Money<C> {}

impl<C: Currency> PartialOrd for Money<C> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<C: Currency> Ord for Money<C> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.amount.cmp(&other.amount)
    }
}

impl<C: Currency> Hash for Money<C> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.amount.hash(state);
    }
}

impl<C: Currency> Money<C> {
    /// Construct from a raw decimal amount (already in minor-unit-appropriate scale).
    pub fn new(amount: Decimal) -> Self {
        Self {
            amount,
            _currency: core::marker::PhantomData,
        }
    }

    /// Construct from a major-unit value (e.g. dollars). Minor-unit precision is
    /// applied via [`Money::round`] when needed; arithmetic is exact.
    pub fn from_major(major: i64) -> Self {
        Self::new(Decimal::from(major))
    }

    /// Construct from a major-unit value given as a decimal.
    pub fn from_major_decimal(major: Decimal) -> Self {
        Self::new(major)
    }

    /// The zero amount for this currency.
    pub fn zero() -> Self {
        Self::new(Decimal::ZERO)
    }

    /// The raw decimal amount.
    pub fn amount(&self) -> Decimal {
        self.amount
    }

    /// The currency marker's ISO code.
    pub fn currency_code(&self) -> &'static str {
        C::CODE
    }

    /// Scale the amount to the currency's minor units, rounding per `strategy`.
    pub fn round(&self, strategy: rust_decimal::RoundingStrategy) -> Self {
        Self::new(self.amount.round_dp_with_strategy(C::MINOR_UNITS, strategy))
    }

    /// Allocate `self` across `ratios` proportionally using the largest-remainder
    /// method, so the parts sum exactly back to `self` at the currency's minor unit.
    /// Panics if `ratios` is empty.
    pub fn allocate(&self, ratios: &[u64]) -> Vec<Money<C>> {
        assert!(!ratios.is_empty(), "allocate requires at least one ratio");
        let total_ratio = Decimal::from(ratios.iter().copied().sum::<u64>());
        let unit = Decimal::from(10).powi(-(C::MINOR_UNITS as i64));

        let mut parts: Vec<Decimal> = vec![Decimal::ZERO; ratios.len()];
        let mut remainders: Vec<(usize, Decimal)> = Vec::with_capacity(ratios.len());
        let mut allocated = Decimal::ZERO;
        for (i, &r) in ratios.iter().enumerate() {
            let share = self.amount * Decimal::from(r);
            let whole = (share / total_ratio)
                .round_dp_with_strategy(C::MINOR_UNITS, RoundingStrategy::ToZero);
            let rem = share - whole * total_ratio;
            parts[i] = whole;
            allocated += whole;
            remainders.push((i, rem));
        }

        let mut leftover = self.amount - allocated;
        remainders.sort_by_key(|&(_, r)| std::cmp::Reverse(r));
        for (i, _) in remainders {
            if leftover >= unit {
                parts[i] += unit;
                leftover -= unit;
            }
        }

        parts.into_iter().map(Money::new).collect()
    }
}

impl<C: Currency> Add for Money<C> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.amount + rhs.amount)
    }
}

impl<C: Currency> Sub for Money<C> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.amount - rhs.amount)
    }
}

impl<C: Currency> AddAssign for Money<C> {
    fn add_assign(&mut self, rhs: Self) {
        self.amount += rhs.amount;
    }
}

impl<C: Currency> SubAssign for Money<C> {
    fn sub_assign(&mut self, rhs: Self) {
        self.amount -= rhs.amount;
    }
}

impl<C: Currency> Neg for Money<C> {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.amount)
    }
}

/// Scale money by a dimensionless decimal scalar (never produces a different currency).
impl<C: Currency> core::ops::Mul<Decimal> for Money<C> {
    type Output = Self;
    fn mul(self, rhs: Decimal) -> Self {
        Self::new(self.amount * rhs)
    }
}

impl<C: Currency> fmt::Display for Money<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", C::SYMBOL, self.amount)
    }
}

impl<C: Currency> Serialize for Money<C> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("Money", 2)?;
        s.serialize_field("amount", &self.amount.to_string())?;
        s.serialize_field("currency", C::CODE)?;
        s.end()
    }
}

impl<'de, C: Currency> Deserialize<'de> for Money<C> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MoneyVisitor<C: Currency>(core::marker::PhantomData<C>);

        impl<'de, C: Currency> Visitor<'de> for MoneyVisitor<C> {
            type Value = Money<C>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a money amount in currency {}", C::CODE)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Money<C>, A::Error> {
                let mut amount: Option<Decimal> = None;
                let mut currency: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "amount" => {
                            let raw = map.next_value::<String>()?;
                            amount =
                                Some(Decimal::from_str_exact(&raw).map_err(de::Error::custom)?);
                        }
                        "currency" => {
                            currency = Some(map.next_value::<String>()?);
                        }
                        other => {
                            return Err(de::Error::unknown_field(other, &["amount", "currency"]));
                        }
                    }
                }
                let amount = amount.ok_or_else(|| de::Error::missing_field("amount"))?;
                let currency = currency.ok_or_else(|| de::Error::missing_field("currency"))?;
                if currency != C::CODE {
                    return Err(de::Error::custom(format!(
                        "expected currency {} but found {}",
                        C::CODE,
                        currency
                    )));
                }
                Ok(Money::new(amount))
            }
        }

        deserializer.deserialize_struct(
            "Money",
            &["amount", "currency"],
            MoneyVisitor(core::marker::PhantomData),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Eur, Usd};
    use rust_decimal_macros::dec;

    #[test]
    fn same_currency_adds() {
        let a = Money::<Usd>::from_major(10);
        let b = Money::<Usd>::from_major(5);
        assert_eq!((a + b).amount(), dec!(15));
    }

    #[test]
    fn zero_is_neutral() {
        let a = Money::<Eur>::from_major(7);
        assert_eq!(a + Money::zero(), a);
        assert_eq!(a - Money::zero(), a);
    }

    #[test]
    fn rounding_to_minor_units() {
        let a = Money::<Usd>::new(dec!(10.456));
        let r = a.round(rust_decimal::RoundingStrategy::MidpointAwayFromZero);
        assert_eq!(r.amount(), dec!(10.46));
    }

    #[test]
    fn allocate_largest_remainder() {
        // $100 split 1:1:1 -> 33.34, 33.33, 33.33
        let total = Money::<Usd>::from_major(100);
        let parts = total.allocate(&[1, 1, 1]);
        assert_eq!(parts[0].amount(), dec!(33.34));
        assert_eq!(parts[1].amount(), dec!(33.33));
        assert_eq!(parts[2].amount(), dec!(33.33));
        let sum: Money<Usd> = parts.into_iter().fold(Money::zero(), |a, b| a + b);
        assert_eq!(sum, total);
    }

    #[test]
    fn display_includes_symbol() {
        assert_eq!(Money::<Usd>::from_major(42).to_string(), "$42");
    }

    #[test]
    fn serde_roundtrip_same_currency() {
        let m = Money::<Eur>::new(dec!(12.50));
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#"{"amount":"12.50","currency":"EUR"}"#);
        let back: Money<Eur> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn serde_rejects_wrong_currency() {
        let json = r#"{"amount":"12.50","currency":"USD"}"#;
        let res: Result<Money<Eur>, _> = serde_json::from_str(json);
        assert!(res.is_err());
    }

    #[test]
    fn jpy_has_no_minor_units() {
        let m = Money::<crate::Jpy>::from_major(1000);
        assert_eq!(m.amount(), Decimal::from(1000));
    }
}
