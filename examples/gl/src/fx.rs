//! Foreign exchange and period-end revaluation.
//!
//! Cross-currency math is kept *typed*: `FxRateTable::convert` turns a `Money<From>`
//! into a `Money<To>` and will not compile if the currencies are swapped. A point-in-time
//! rate table answers "what was the rate on this date?" so historical postings keep their
//! original translation while period-end revaluation re-marks foreign balances to the
//! closing rate and books the difference to a FX gain/loss account.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::HashMap;
use tpt_erp_ledger::{AccountId, EntrySide, LedgerEntry};
use tpt_erp_primitives::{Currency, Money};

use crate::journal::{JournalEngine, JournalError};

/// A point-in-time FX rate table.
///
/// Rates are stored per `(from, to)` currency pair as a chronological list of
/// `(as_of, rate)` observations. `rate` returns the most recent rate at or before the
/// query time, so an old posting keeps the rate that was effective when it happened.
#[derive(Debug, Clone, Default)]
pub struct FxRateTable {
    /// `(from, to)` currency pair -> chronological (as_of, rate) observations.
    #[allow(clippy::type_complexity)]
    rates: HashMap<(String, String), Vec<(DateTime<Utc>, Decimal)>>,
}

impl FxRateTable {
    /// An empty rate table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a rate of `from` denominated in `to` effective at `as_of`.
    pub fn set_rate(&mut self, from: &str, to: &str, as_of: DateTime<Utc>, rate: Decimal) {
        self.rates
            .entry((from.to_string(), to.to_string()))
            .or_default()
            .push((as_of, rate));
    }

    /// Rate of `from` in terms of `to` at or before `as_of`. Identity (1.0) for `from == to`.
    pub fn rate(&self, from: &str, to: &str, as_of: DateTime<Utc>) -> Option<Decimal> {
        if from == to {
            return Some(Decimal::ONE);
        }
        let hist = self.rates.get(&(from.to_string(), to.to_string()))?;
        hist.iter()
            .filter(|(t, _)| *t <= as_of)
            .max_by_key(|(t, _)| *t)
            .map(|(_, r)| *r)
    }

    /// Typed cross-currency conversion: `Money<From>` -> `Money<To>`.
    pub fn convert<F, T>(&self, amount: Money<F>, as_of: DateTime<Utc>) -> Option<Money<T>>
    where
        F: Currency,
        T: Currency,
    {
        let r = self.rate(F::CODE, T::CODE, as_of)?;
        Some(Money::<T>::new(amount.amount() * r))
    }
}

/// The adjustment booked for one foreign-currency account at revaluation.
#[derive(Debug, Clone)]
pub struct RevaluationAdjustment<F: Currency, R: Currency> {
    pub account: AccountId,
    pub foreign_balance: Money<F>,
    pub book_local: Money<R>,
    pub closing_local: Money<R>,
    /// `closing_local - book_local` — the gain (positive) or loss (negative).
    pub adjustment: Money<R>,
}

fn opposite(side: EntrySide) -> EntrySide {
    match side {
        EntrySide::Debit => EntrySide::Credit,
        EntrySide::Credit => EntrySide::Debit,
    }
}

/// Period-end revaluation.
///
/// For every foreign-currency account (`F`) with activity, translate its balance into the
/// reporting currency (`R`) at the closing rate, compare to its book (historical) rate,
/// and post the difference in the *reporting* journal. The foreign-side leg is booked to
/// `revaluation_account` (a reporting-currency account), never to the original foreign
/// account id — posting a foreign account id into the reporting ledger would create a
/// phantom account that corrupts multi-entity consolidation. The other leg goes to
/// `fx_account`. The foreign account's book rate is then updated to the closing rate.
pub async fn revalue<F, R>(
    foreign: &JournalEngine<F>,
    reporting: &mut JournalEngine<R>,
    table: &FxRateTable,
    as_of: DateTime<Utc>,
    revaluation_account: AccountId,
    fx_account: AccountId,
    period: &str,
) -> Result<Vec<RevaluationAdjustment<F, R>>, JournalError>
where
    F: Currency + Serialize + serde::de::DeserializeOwned,
    R: Currency + Serialize + serde::de::DeserializeOwned,
{
    let mut out = Vec::new();
    let closing_rate = match table.rate(F::CODE, R::CODE, as_of) {
        Some(r) => r,
        None => return Ok(out),
    };

    for acc in foreign.chart_of_accounts().iter() {
        let bal = foreign.balance_of(acc.id);
        let foreign_balance = bal.signed(acc.kind);
        if foreign_balance == Money::zero() {
            continue;
        }
        let book_rate = match foreign.book_rate(acc.id) {
            Some(r) => r,
            None => continue,
        };

        let closing_local = match table.convert::<F, R>(foreign_balance, as_of) {
            Some(m) => m,
            None => continue,
        };
        let book_local = Money::<R>::new(foreign_balance.amount() * book_rate);
        let adjustment = closing_local - book_local;
        if adjustment == Money::zero() {
            continue;
        }

        let amount = if adjustment < Money::zero() {
            -adjustment
        } else {
            adjustment
        };
        let foreign_side = if adjustment >= Money::zero() {
            acc.kind.normal_side()
        } else {
            opposite(acc.kind.normal_side())
        };
        let entries = vec![
            LedgerEntry {
                // Reporting-currency revaluation account, NOT the foreign account id.
                account: revaluation_account,
                side: foreign_side,
                amount,
            },
            LedgerEntry {
                account: fx_account,
                side: opposite(foreign_side),
                amount,
            },
        ];
        reporting
            .post_transaction(entries, period, "FX revaluation")
            .await?;

        foreign.set_book_rate(acc.id, closing_rate);

        out.push(RevaluationAdjustment {
            account: acc.id,
            foreign_balance,
            book_local,
            closing_local,
            adjustment,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use tpt_erp_ledger::{EntrySide, LedgerEntry};
    use tpt_erp_primitives::{Currency, Eur, Money, Usd};

    fn leg<C: Currency>(account: AccountId, side: EntrySide, amount: i64) -> LedgerEntry<C> {
        LedgerEntry {
            account,
            side,
            amount: Money::<C>::new(Decimal::from(amount)),
        }
    }

    #[test]
    fn typed_conversion_uses_point_in_time_rate() {
        let mut table = FxRateTable::new();
        let early = Utc::now();
        let later = early + chrono::Duration::days(1);
        table.set_rate("EUR", "USD", early, Decimal::new(110, 2)); // 1.10
        table.set_rate("EUR", "USD", later, Decimal::new(120, 2)); // 1.20

        let eur = Money::<Eur>::new(Decimal::from(100));
        // Rate moves with `as_of` — historical translations stay stable.
        assert_eq!(
            table.convert::<Eur, Usd>(eur, early).unwrap().amount(),
            Decimal::from(110)
        );
        assert_eq!(
            table.convert::<Eur, Usd>(eur, later).unwrap().amount(),
            Decimal::from(120)
        );
        // Same-currency is identity.
        assert_eq!(table.rate("USD", "USD", later), Some(Decimal::ONE));
    }

    #[tokio::test]
    async fn revaluation_posts_balanced_adjustment() {
        let now = Utc::now();
        let mut table = FxRateTable::new();
        table.set_rate("EUR", "USD", now, Decimal::new(110, 2)); // 1.10

        // Foreign (EUR) ledger.
        let (foreign_coa, fd) = crate::coa::demo_coa::<Eur>();
        let foreign = JournalEngine::<Eur>::new(crate::demo_tenant(), foreign_coa);
        foreign.set_book_rate(fd.cash, Decimal::ONE); // opened at par
        foreign
            .post_transaction(
                vec![
                    leg(fd.cash, EntrySide::Debit, 100),
                    leg(fd.common_stock, EntrySide::Credit, 100),
                ],
                "2026-01",
                "invest",
            )
            .await
            .unwrap();

        // Reporting (USD) ledger.
        let (rcoa, rd) = crate::coa::demo_coa::<Usd>();
        let mut reporting = JournalEngine::<Usd>::new(crate::demo_tenant(), rcoa);

        let adj = revalue(
            &foreign,
            &mut reporting,
            &table,
            now,
            rd.cum_translation_adjustment,
            rd.fx_gain_loss,
            "2026-01",
        )
        .await
        .unwrap();

        // 100 EUR * 1.10 = 110 USD; book 100 => +10 gain.
        assert_eq!(adj.len(), 1);
        let a = &adj[0];
        assert_eq!(a.closing_local.amount(), Decimal::from(110));
        assert_eq!(a.book_local.amount(), Decimal::from(100));
        assert_eq!(a.adjustment.amount(), Decimal::from(10));

        // Revaluation posts to a reporting-currency account (NOT the foreign account id):
        // CTA debited +10, FX gain/loss credited +10 => balanced and consolidation-safe.
        assert_eq!(
            reporting
                .balance_of(rd.cum_translation_adjustment)
                .debits
                .amount(),
            Decimal::from(10)
        );
        assert_eq!(
            reporting.balance_of(rd.fx_gain_loss).credits.amount(),
            Decimal::from(10)
        );
    }
}
