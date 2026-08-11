//! Streaming anomaly detection over the ledger event stream.
//!
//! Anomaly detection is just another [`Projector`]: it folds the same [`LedgerEvent`] stream
//! the balance read model consumes, but instead of maintaining balances it maintains a set of
//! streaming detectors and emits [`Anomaly`] findings. Because it is a projector, it runs over
//! the live event stream *and* over a historical replay, so the same detector backs both
//! real-time alerting and post-hoc audit.
//!
//! Detectors:
//!
//! * **Unbalanced transaction** — defense in depth: a transaction that reaches the projector
//!   without balancing is flagged (the projection layer already rejects these, but detection
//!   never assumes the upstream guard ran).
//! * **Duplicate transaction** — the same set of entries posted twice (same payees/amounts)
//!   is a classic double-post; flagged with the id of the first occurrence.
//! * **Amount outlier** — per-transaction absolute value is tracked with Welford's online
//!   mean/variance; a value more than `z_threshold` standard deviations from the running mean
//!   is flagged. The statistic is *streaming*, so it needs no second pass and works over an
//!   unbounded event stream.

    use crate::double_entry::{DoubleEntry, EntrySide, LedgerEvent, TransactionId};
    use crate::projection::{ProjectionError, Projector};
    use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::HashMap;
use tpt_erp_primitives::{Currency, Money};

/// A detected anomaly on the ledger stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Anomaly<C: Currency> {
    /// A transaction reached the detector without balancing.
    Unbalanced { tx: TransactionId },
    /// The same entries were posted before (double-post / replay fraud).
    Duplicate {
        tx: TransactionId,
        first: TransactionId,
    },
    /// A transaction whose absolute value is a statistical outlier vs. the running stream.
    AmountOutlier {
        tx: TransactionId,
        amount: Money<C>,
        z_score: f64,
    },
}

/// Streaming anomaly detector over [`LedgerEvent`]s.
#[derive(Debug, Clone)]
pub struct AnomalyProjector<C: Currency> {
    /// Findings accumulated so far (in stream order).
    pub anomalies: Vec<Anomaly<C>>,
    /// Welford running moments of per-transaction absolute value.
    n: u64,
    mean: f64,
    m2: f64,
    /// Stable signature of already-seen entry sets -> first transaction id.
    seen: HashMap<String, TransactionId>,
    /// Standard-deviation threshold for the amount-outlier detector.
    z_threshold: f64,
    /// Minimum sample size before outlier detection switches on.
    min_samples: u64,
}

impl<C: Currency> AnomalyProjector<C> {
    /// A detector with the default thresholds (`z = 3.0`, `min_samples = 8`).
    pub fn new() -> Self {
        Self::with_config(3.0, 8)
    }

    /// A detector with explicit `z_threshold` and `min_samples`.
    pub fn with_config(z_threshold: f64, min_samples: u64) -> Self {
        Self {
            anomalies: Vec::new(),
            n: 0,
            mean: 0.0,
            m2: 0.0,
            seen: HashMap::new(),
            z_threshold,
            min_samples,
        }
    }

    /// Stable signature of a transaction's entry set (order-independent).
    fn signature(tx: &crate::double_entry::Transaction<C>) -> String {
        let mut parts: Vec<String> = tx
            .entries
            .iter()
            .map(|e| {
                format!(
                    "{}:{}:{}",
                    e.account.as_str(),
                    match e.side {
                        EntrySide::Debit => 'D',
                        EntrySide::Credit => 'C',
                    },
                    e.amount.amount()
                )
            })
            .collect();
        parts.sort();
        parts.join("|")
    }

    fn detect_amount(&mut self, tx: TransactionId, value: f64) {
        // Score this sample against the running distribution built from *prior* samples,
        // so the candidate itself cannot inflate the standard deviation and mask its own
        // outlier status (a single huge value would otherwise pull std up and push its z down).
        if self.n >= self.min_samples {
            let variance = self.m2 / (self.n as f64);
            let std = variance.sqrt();
            if std > 0.0 {
                let z = (value - self.mean) / std;
                if z.abs() > self.z_threshold {
                    self.anomalies.push(Anomaly::AmountOutlier {
                        tx,
                        amount: Money::<C>::new(Decimal::try_from(value as i64).unwrap_or(Decimal::ZERO)),
                        z_score: z,
                    });
                }
            }
        }
        // Welford's online update of mean and variance (now including this sample).
        self.n += 1;
        let delta = value - self.mean;
        self.mean += delta / self.n as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
    }
}

impl<C: Currency> Default for AnomalyProjector<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Currency> Projector for AnomalyProjector<C> {
    type Event = LedgerEvent<C>;

    async fn apply(&mut self, event: &LedgerEvent<C>) -> Result<(), ProjectionError> {
        match event {
            LedgerEvent::TransactionPosted(tx) => {
                // 1. Balance guard (defense in depth).
                if !tx.is_balanced() {
                    self.anomalies.push(Anomaly::Unbalanced { tx: tx.id });
                    return Ok(());
                }
                // 2. Duplicate detection.
                let sig = Self::signature(tx);
                if let Some(&first) = self.seen.get(&sig) {
                    if first != tx.id {
                        self.anomalies.push(Anomaly::Duplicate { tx: tx.id, first });
                    }
                } else {
                    self.seen.insert(sig, tx.id);
                }
                // 3. Amount outlier (streaming z-score).
                let value: f64 = tx
                    .entries
                    .iter()
                    .map(|e| e.amount.amount().to_f64().unwrap_or(0.0))
                    .sum();
                self.detect_amount(tx.id, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_entry::{EntrySide, LedgerEntry, LedgerEvent, Transaction};
    use rust_decimal::Decimal;
    use tpt_erp_primitives::Usd;

    fn tx(amounts: &[i64]) -> LedgerEvent<Usd> {
        // Build a balanced transaction: half the amounts as debits, half as credits (or
        // pair them). For tests we make a simple 2-entry balanced tx per amount.
        let mut entries = Vec::new();
        let mut d = 0;
        let mut c = 0;
        for &a in amounts {
            if d <= c {
                entries.push(LedgerEntry {
                    account: crate::AccountId::new(),
                    side: EntrySide::Debit,
                    amount: Money::new(Decimal::from(a)),
                });
                d += a;
            } else {
                entries.push(LedgerEntry {
                    account: crate::AccountId::new(),
                    side: EntrySide::Credit,
                    amount: Money::new(Decimal::from(a)),
                });
                c += a;
            }
        }
        LedgerEvent::TransactionPosted(Transaction {
            id: TransactionId::new(),
            entries,
        })
    }

    /// A balanced transaction whose entry accounts are supplied by the caller, so two
    /// identical calls produce the same duplicate signature (real double-post detection).
    fn tx_with(debit_acct: crate::AccountId, credit_acct: crate::AccountId, amt: i64) -> LedgerEvent<Usd> {
        LedgerEvent::TransactionPosted(Transaction {
            id: TransactionId::new(),
            entries: vec![
                LedgerEntry {
                    account: debit_acct,
                    side: EntrySide::Debit,
                    amount: Money::new(Decimal::from(amt)),
                },
                LedgerEntry {
                    account: credit_acct,
                    side: EntrySide::Credit,
                    amount: Money::new(Decimal::from(amt)),
                },
            ],
        })
    }

    #[tokio::test]
    async fn flags_unbalanced_transaction() {
        // A deliberately unbalanced 1-entry "transaction" (debit only) is flagged.
        let mut proj = AnomalyProjector::<Usd>::new();
        let ev = LedgerEvent::TransactionPosted(Transaction {
            id: TransactionId::new(),
            entries: vec![LedgerEntry {
                account: crate::AccountId::new(),
                side: EntrySide::Debit,
                amount: Money::new(Decimal::from(100)),
            }],
        });
        proj.apply(&ev).await.unwrap();
        assert!(matches!(proj.anomalies[0], Anomaly::Unbalanced { .. }));
    }

    #[tokio::test]
    async fn flags_duplicate_posting() {
        let mut proj = AnomalyProjector::<Usd>::with_config(10.0, 1_000_000);
        // Two balanced transactions with identical entry sets (same accounts) = a double-post.
        let d = crate::AccountId::new();
        let c = crate::AccountId::new();
        let e1 = tx_with(d, c, 50);
        let e2 = tx_with(d, c, 50);
        proj.apply(&e1).await.unwrap();
        proj.apply(&e2).await.unwrap();
        assert!(
            proj.anomalies
                .iter()
                .any(|a| matches!(a, Anomaly::Duplicate { .. }))
        );
    }

    #[tokio::test]
    async fn flags_amount_outlier() {
        let mut proj = AnomalyProjector::<Usd>::with_config(3.0, 4);
        // A run of small, similar transactions establishes the baseline.
        for amt in [10, 12, 11, 13, 10] {
            proj.apply(&tx(&[amt, amt])).await.unwrap();
        }
        // A massive spike should be flagged as an outlier.
        proj.apply(&tx(&[10_000, 10_000])).await.unwrap();
        assert!(
            proj.anomalies
                .iter()
                .any(|a| matches!(a, Anomaly::AmountOutlier { .. }))
        );
    }

    #[tokio::test]
    async fn normal_stream_stays_clean() {
        let mut proj = AnomalyProjector::<Usd>::with_config(3.0, 4);
        for amt in [100, 102, 98, 101, 99, 100, 103, 97] {
            proj.apply(&tx(&[amt, amt])).await.unwrap();
        }
        // No outlier among a tight, consistent cluster.
        assert!(
            !proj
                .anomalies
                .iter()
                .any(|a| matches!(a, Anomaly::AmountOutlier { .. }))
        );
    }
}
