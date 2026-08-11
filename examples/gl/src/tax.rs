//! Richer, point-in-time tax modeling (beyond the flat demo tier in `examples/plugins/tax`).
//!
//! The demo tax plugin keys a single basis-point rate off a jurisdiction *string* and has no
//! effective-date dimension. Here tax is a first-class, pure model:
//!
//! - [`TaxRateTable`] is a rate table keyed by `(jurisdiction, tax_type, effective_date)`.
//!   [`TaxRateTable::rate`] answers "what was the rate on this date?" so a sale posted in 2020
//!   is taxed at the 2020 rate even after a 2024 hike.
//! - [`compute_tax`] folds a set of posted journal lines (each tagged with a jurisdiction and
//!   tax type) into per-jurisdiction tax-payable totals. All math is [`Decimal`]/[`Money`]
//!   based — there is no `f64` and no network.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tpt_erp_ledger::AccountId;
use tpt_erp_primitives::{Currency, Money};

/// A tax jurisdiction (e.g. `"US-CA"`, `"EU-DE"`, `"JP"`). Newtype so it cannot be confused
/// with an arbitrary string elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Jurisdiction(pub String);

impl Jurisdiction {
    /// Build a jurisdiction from a string-like value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// The kind of tax a rate line applies to. Used together with the jurisdiction and effective
/// date to select a rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaxType {
    /// Sales / use tax on revenue.
    Sales,
    /// Value-added tax (a liability collected on behalf of the authority).
    Vat,
    /// Corporate income tax.
    Corporate,
    /// Withholding tax on outbound payments.
    Withholding,
}

/// One applicable tax rate: a fractional rate (e.g. `0.0725` for 7.25%) and the date it became
/// effective. Stored in a chronological list per `(jurisdiction, tax_type)`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TaxRateRow {
    /// The rate as a decimal fraction (not basis points), e.g. `0.0725`.
    pub rate: Decimal,
    /// The instant this rate became effective.
    pub effective: DateTime<Utc>,
    /// Human-readable note (e.g. "2024 hike").
    pub note: &'static str,
}

/// A point-in-time tax rate table keyed by `(jurisdiction, tax_type, effective_date)`.
///
/// Rates for the same key are kept in chronological order so [`TaxRateTable::rate`] can return
/// the most recent rate at or before a query date.
#[derive(Debug, Clone, Default)]
pub struct TaxRateTable {
    #[allow(clippy::type_complexity)]
    rates: HashMap<(Jurisdiction, TaxType), Vec<TaxRateRow>>,
}

impl TaxRateTable {
    /// An empty rate table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a rate effective at `effective`. Duplicate effective dates collapse to the last
    /// one inserted (deterministic: later `insert` wins).
    pub fn insert(
        &mut self,
        jurisdiction: Jurisdiction,
        tax_type: TaxType,
        effective: DateTime<Utc>,
        rate: Decimal,
        note: &'static str,
    ) {
        let entry = TaxRateRow {
            rate,
            effective,
            note,
        };
        let list = self.rates.entry((jurisdiction, tax_type)).or_default();
        if let Some(slot) = list.iter_mut().find(|r| r.effective == effective) {
            *slot = entry;
        } else {
            list.push(entry);
            // Keep chronological for stable point-in-time lookup.
            list.sort_by_key(|r| r.effective);
        }
    }

    /// The rate in effect at or before `as_of` for the given `(jurisdiction, tax_type)`.
    /// Returns `None` if no rate existed on or before that date.
    pub fn rate(
        &self,
        jurisdiction: &Jurisdiction,
        tax_type: TaxType,
        as_of: DateTime<Utc>,
    ) -> Option<&TaxRateRow> {
        let hist = self.rates.get(&(jurisdiction.clone(), tax_type))?;
        hist.iter()
            .filter(|r| r.effective <= as_of)
            .max_by_key(|r| r.effective)
    }
}

/// One taxable line: a journal amount attributed to a jurisdiction and tax type.
///
/// In practice this is built from a posted journal line set filtered to revenue/VAT-relevant
/// accounts; the `account` is carried only for traceability and is not used in the math.
#[derive(Debug, Clone)]
pub struct TaxableLine<C: Currency> {
    pub jurisdiction: Jurisdiction,
    pub tax_type: TaxType,
    pub account: Option<AccountId>,
    /// The taxable base (e.g. net sales) in the ledger currency `C`.
    pub base: Money<C>,
}

/// Per-jurisdiction tax computation result.
#[derive(Debug, Clone)]
pub struct TaxByJurisdiction<C: Currency> {
    pub jurisdiction: Jurisdiction,
    /// Sum of the taxable bases seen for this jurisdiction.
    pub taxable_base: Money<C>,
    /// Tax payable for this jurisdiction (bases times rate, rounded to minor units).
    pub tax_payable: Money<C>,
}

/// The full result of [`compute_tax`].
#[derive(Debug, Clone)]
pub struct TaxResult<C: Currency> {
    pub by_jurisdiction: Vec<TaxByJurisdiction<C>>,
    pub total_base: Money<C>,
    pub total_tax: Money<C>,
}

/// Errors raised while computing tax.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaxError {
    #[error("no tax rate for jurisdiction {jurisdiction:?} / {tax_type:?} at the given date")]
    NoRate {
        jurisdiction: Jurisdiction,
        tax_type: TaxType,
    },
}

/// Compute tax payable per jurisdiction for a set of posted taxable lines, using the rate in
/// effect at `as_of` for each line.
///
/// Each line's tax is `base * rate`, rounded to the currency's minor units with the
/// midpoint-away-from-zero strategy; the rounded tax (not the raw base) is what accrues, so
/// the per-jurisdiction totals exactly equal the sum of the per-line taxes. The result is
/// grouped by jurisdiction; lines sharing a jurisdiction are summed before reporting.
///
/// # Errors
/// Returns [`TaxError::NoRate`] if any line's `(jurisdiction, tax_type)` has no effective rate
/// on or before `as_of`. Callers should ensure the rate table covers every referenced pair.
pub fn compute_tax<C: Currency>(
    table: &TaxRateTable,
    lines: &[TaxableLine<C>],
    as_of: DateTime<Utc>,
) -> Result<TaxResult<C>, TaxError> {
    // Accumulate per-jurisdiction (base, tax) before rounding the totals, so the reported
    // total equals the sum of per-line rounded taxes.
    let mut per_jur: HashMap<Jurisdiction, (Decimal, Decimal)> = HashMap::new();
    let mut total_base = Decimal::ZERO;
    let mut total_tax = Decimal::ZERO;

    for line in lines {
        let row = table
            .rate(&line.jurisdiction, line.tax_type, as_of)
            .ok_or(TaxError::NoRate {
                jurisdiction: line.jurisdiction.clone(),
                tax_type: line.tax_type,
            })?;
        let raw_tax = line.base.amount() * row.rate;
        let rounded = Money::<C>::new(raw_tax).round(RoundingStrategy::MidpointAwayFromZero);
        let tax_dec = rounded.amount();

        let entry = per_jur
            .entry(line.jurisdiction.clone())
            .or_insert((Decimal::ZERO, Decimal::ZERO));
        entry.0 += line.base.amount();
        entry.1 += tax_dec;

        total_base += line.base.amount();
        total_tax += tax_dec;
    }

    let mut by_jurisdiction: Vec<TaxByJurisdiction<C>> = per_jur
        .into_iter()
        .map(|(jurisdiction, (base, tax))| TaxByJurisdiction {
            jurisdiction,
            taxable_base: Money::<C>::new(base),
            tax_payable: Money::<C>::new(tax),
        })
        .collect();
    by_jurisdiction.sort_by(|a, b| a.jurisdiction.0.cmp(&b.jurisdiction.0));

    Ok(TaxResult {
        by_jurisdiction,
        total_base: Money::<C>::new(total_base),
        total_tax: Money::<C>::new(total_tax),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rust_decimal::Decimal;
    use tpt_erp_primitives::Usd;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).single().unwrap()
    }

    fn line(jur: &str, tax_type: TaxType, base: i64) -> TaxableLine<Usd> {
        TaxableLine {
            jurisdiction: Jurisdiction::new(jur),
            tax_type,
            account: None,
            base: Money::<Usd>::new(Decimal::from(base)),
        }
    }

    #[test]
    fn point_in_time_rate_lookup() {
        let mut table = TaxRateTable::new();
        let jur = Jurisdiction::new("US-CA");
        let t = TaxType::Sales;
        table.insert(
            jur.clone(),
            t,
            at(2020, 1, 1),
            Decimal::new(725, 4),
            "2020 base",
        ); // 7.25%
        table.insert(
            jur.clone(),
            t,
            at(2024, 1, 1),
            Decimal::new(950, 4),
            "2024 hike",
        ); // 9.50%

        // Before the first rate: none.
        assert!(table.rate(&jur, t, at(2019, 6, 1)).is_none());
        // Between the two: the 2020 rate still applies.
        assert_eq!(
            table.rate(&jur, t, at(2023, 6, 1)).unwrap().rate,
            Decimal::new(725, 4)
        );
        // After the hike: the 2024 rate applies.
        assert_eq!(
            table.rate(&jur, t, at(2025, 1, 1)).unwrap().rate,
            Decimal::new(950, 4)
        );
    }

    #[test]
    fn compute_tax_per_jurisdiction() {
        let mut table = TaxRateTable::new();
        // US-CA sales 10%, EU-DE VAT 20%.
        table.insert(
            Jurisdiction::new("US-CA"),
            TaxType::Sales,
            at(2020, 1, 1),
            Decimal::new(10, 2),
            "ca",
        );
        table.insert(
            Jurisdiction::new("EU-DE"),
            TaxType::Vat,
            at(2020, 1, 1),
            Decimal::new(20, 2),
            "de",
        );

        let lines = vec![
            line("US-CA", TaxType::Sales, 100),
            line("US-CA", TaxType::Sales, 200),
            line("EU-DE", TaxType::Vat, 500),
        ];
        let res = compute_tax(&table, &lines, at(2026, 1, 1)).unwrap();

        assert_eq!(res.total_base.amount(), Decimal::from(800));
        // CA: (100+200)*0.10 = 30; DE: 500*0.20 = 100; total 130.
        assert_eq!(res.total_tax.amount(), Decimal::from(130));

        let ca = res
            .by_jurisdiction
            .iter()
            .find(|x| x.jurisdiction.0 == "US-CA")
            .unwrap();
        assert_eq!(ca.taxable_base.amount(), Decimal::from(300));
        assert_eq!(ca.tax_payable.amount(), Decimal::from(30));
        let de = res
            .by_jurisdiction
            .iter()
            .find(|x| x.jurisdiction.0 == "EU-DE")
            .unwrap();
        assert_eq!(de.tax_payable.amount(), Decimal::from(100));
    }

    #[test]
    fn tax_uses_point_in_time_rate() {
        let mut table = TaxRateTable::new();
        let jur = Jurisdiction::new("US-CA");
        table.insert(
            jur.clone(),
            TaxType::Sales,
            at(2020, 1, 1),
            Decimal::new(700, 4),
            "7%",
        );
        table.insert(
            jur.clone(),
            TaxType::Sales,
            at(2024, 1, 1),
            Decimal::new(900, 4),
            "9%",
        );

        // A 2023 sale is taxed at 7%; a 2025 sale at 9% (same base of 1000).
        let before: TaxableLine<Usd> = TaxableLine {
            jurisdiction: jur.clone(),
            tax_type: TaxType::Sales,
            account: None,
            base: Money::<Usd>::new(Decimal::from(1000)),
        };
        let after = before.clone();
        let r1 = compute_tax(&table, &[before], at(2023, 1, 1)).unwrap();
        let r2 = compute_tax(&table, &[after], at(2025, 1, 1)).unwrap();
        assert_eq!(r1.total_tax.amount(), Decimal::from(70));
        assert_eq!(r2.total_tax.amount(), Decimal::from(90));
    }

    #[test]
    fn tax_rounds_to_minor_units() {
        let mut table = TaxRateTable::new();
        // 7.5% on odd bases; rounding to cents must be exact and additive.
        table.insert(
            Jurisdiction::new("X"),
            TaxType::Sales,
            at(2020, 1, 1),
            Decimal::new(75, 3),
            "7.5%",
        );

        // 33.33 * 0.075 = 2.49975 -> 2.50; 66.67 * 0.075 = 5.00025 -> 5.00.
        let lines = vec![
            TaxableLine {
                jurisdiction: Jurisdiction::new("X"),
                tax_type: TaxType::Sales,
                account: None,
                base: Money::<Usd>::new(Decimal::from_str_exact("33.33").unwrap()),
            },
            TaxableLine {
                jurisdiction: Jurisdiction::new("X"),
                tax_type: TaxType::Sales,
                account: None,
                base: Money::<Usd>::new(Decimal::from_str_exact("66.67").unwrap()),
            },
        ];
        let res = compute_tax(&table, &lines, at(2026, 1, 1)).unwrap();
        assert_eq!(
            res.by_jurisdiction[0].tax_payable.amount(),
            Decimal::from_str_exact("7.50").unwrap()
        );
        assert_eq!(
            res.total_tax.amount(),
            Decimal::from_str_exact("7.50").unwrap()
        );
    }

    #[test]
    fn missing_rate_errors() {
        let table = TaxRateTable::new();
        let lines = vec![line("NOWHERE", TaxType::Sales, 100)];
        assert!(matches!(
            compute_tax(&table, &lines, at(2026, 1, 1)),
            Err(TaxError::NoRate { .. })
        ));
    }
}
