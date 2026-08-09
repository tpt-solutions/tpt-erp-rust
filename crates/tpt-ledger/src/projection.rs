//! CQRS projection engine.
//!
//! Read models are built by replaying the event log through a [`Projector`]. Because the
//! log is the source of truth, any read model can be *rebuilt from scratch* by replaying
//! all events — there is no risk of drift.

use crate::AccountId;
use crate::double_entry::{EntrySide, LedgerEvent};
use std::collections::HashMap;
use thiserror::Error;
use tpt_primitives::{Currency, Money};

/// Errors raised while running a projection.
#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("failed to deserialize event payload: {0}")]
    Deserialize(#[from] serde_json::Error),
}

/// A read-model builder that folds events into state.
pub trait Projector {
    /// The event type this projector consumes.
    type Event;

    /// Fold a single event into the read model.
    #[allow(async_fn_in_trait)]
    async fn apply(&mut self, event: &Self::Event) -> Result<(), ProjectionError>;
}

/// Rebuild a projector from scratch by folding every event in order.
pub async fn replay<P, I, E>(mut projector: P, events: I) -> Result<P, ProjectionError>
where
    P: Projector<Event = E>,
    I: IntoIterator<Item = E>,
    E: Send,
{
    for ev in events {
        projector.apply(&ev).await?;
    }
    Ok(projector)
}

/// Example read model: the running balance of every account.
///
/// We treat a `Debit` as decreasing the balance and a `Credit` as increasing it. The
/// projection is fully reconstructed from the event log, so it can never silently
/// disagree with the ledger of record.
#[derive(Debug, Clone)]
pub struct BalanceProjection<C: Currency> {
    pub balances: HashMap<AccountId, Money<C>>,
}

impl<C: Currency> Default for BalanceProjection<C> {
    fn default() -> Self {
        Self {
            balances: HashMap::new(),
        }
    }
}

impl<C: Currency> BalanceProjection<C> {
    pub fn balance_of(&self, account: &AccountId) -> Money<C> {
        self.balances
            .get(account)
            .copied()
            .unwrap_or_else(Money::zero)
    }
}

impl<C: Currency> Projector for BalanceProjection<C> {
    type Event = LedgerEvent<C>;

    async fn apply(&mut self, event: &LedgerEvent<C>) -> Result<(), ProjectionError> {
        match event {
            LedgerEvent::TransactionPosted(tx) => {
                for entry in &tx.entries {
                    let bal = self
                        .balances
                        .entry(entry.account)
                        .or_insert_with(Money::zero);
                    *bal = match entry.side {
                        EntrySide::Debit => *bal - entry.amount,
                        EntrySide::Credit => *bal + entry.amount,
                    };
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_entry::{EntrySide, LedgerEntry, LedgerEvent, Transaction, TransactionId};
    use rust_decimal::Decimal;
    use tpt_primitives::Usd;

    fn posted(debit_amt: i64, credit_amt: i64) -> (AccountId, AccountId, LedgerEvent<Usd>) {
        let d = AccountId::new();
        let c = AccountId::new();
        let tx = Transaction {
            id: TransactionId::new(),
            entries: vec![
                LedgerEntry {
                    account: d,
                    side: EntrySide::Debit,
                    amount: Money::new(Decimal::from(debit_amt)),
                },
                LedgerEntry {
                    account: c,
                    side: EntrySide::Credit,
                    amount: Money::new(Decimal::from(credit_amt)),
                },
            ],
        };
        (d, c, LedgerEvent::TransactionPosted(tx))
    }

    #[tokio::test]
    async fn replay_builds_correct_balances() {
        let (d, c, ev1) = posted(100, 100);
        let (d2, _c2, ev2) = posted(50, 50);

        let proj = BalanceProjection::<Usd>::default();
        let proj = replay(proj, [ev1, ev2]).await.unwrap();

        // Each debit account decreases by its amount.
        assert_eq!(proj.balance_of(&d).amount(), Decimal::from(-100));
        assert_eq!(proj.balance_of(&d2).amount(), Decimal::from(-50));
        // The first credit account gained 100.
        assert_eq!(proj.balance_of(&c).amount(), Decimal::from(100));
    }

    #[tokio::test]
    async fn rebuild_from_scratch_equals_incremental() {
        let (d, c, ev1) = posted(100, 100);
        let (_, _c2, ev2) = posted(50, 50);
        let (d3, _, ev3) = posted(25, 25);

        // Incremental: apply events one batch at a time.
        let mut incremental = BalanceProjection::<Usd>::default();
        incremental.apply(&ev1).await.unwrap();
        incremental.apply(&ev2).await.unwrap();
        incremental.apply(&ev3).await.unwrap();

        // From scratch: replay the full ordered log.
        let from_scratch = replay(BalanceProjection::<Usd>::default(), [ev1, ev2, ev3])
            .await
            .unwrap();

        assert_eq!(incremental.balance_of(&d), from_scratch.balance_of(&d));
        assert_eq!(incremental.balance_of(&c), from_scratch.balance_of(&c));
        assert_eq!(incremental.balance_of(&d3), from_scratch.balance_of(&d3));
    }

    #[tokio::test]
    async fn projection_reads_from_stored_event_payload() {
        // Exercise the event-store -> projection path end to end.
        let (d, _, ev) = posted(70, 70);
        let json = serde_json::to_value(&ev).unwrap();
        let ledger_ev = LedgerEvent::<Usd>::from_payload(&json).unwrap();

        let proj = replay(BalanceProjection::<Usd>::default(), [ledger_ev])
            .await
            .unwrap();
        assert_eq!(proj.balance_of(&d).amount(), Decimal::from(-70));
    }
}
