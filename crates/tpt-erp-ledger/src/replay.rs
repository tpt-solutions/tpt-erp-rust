//! Point-in-time replay of the event log.
//!
//! The ledger is the system of record, so any read model can be rebuilt from scratch by
//! replaying events. *Point-in-time* replay goes one step further: it rebuilds a read model
//! **as it appeared after the first `n` appends to the global log**, which is exactly the
//! historical view an auditor or "time-travel" UI needs. Combine it with any [`Projector`]
//! to answer "what did the balances look like at close of business on day X?" without a
//! single mutating query.
//!
//! The global log order (the append sequence) is the canonical time axis. Because every
//! per-aggregate `sequence` is derived from this same ordered log, replaying the first `n`
//! events reproduces the entire system state at that instant deterministically.

use crate::{EventStore, StoredEvent};
use crate::projection::{ProjectionError, Projector};

/// Rebuild `projector` from the first `upto_index` events of `store`'s global log.
///
/// `upto_index` is a 0-based exclusive bound on the global append position, so passing `k`
/// yields the read model exactly as it was *after* the `k`-th event was appended. Values
/// past the end of the log are clamped to the log length (replaying everything).
///
/// `convert` turns a stored event into the projector's event type (e.g. deserializing a
/// [`StoredEvent`] payload into a [`crate::double_entry::LedgerEvent`]); the same conversion
/// boundary the rest of the framework uses for replay-from-scratch.
///
/// [`EventStore`]: crate::event::EventStore
pub async fn replay_point_in_time<P, A, E, F>(
    mut projector: P,
    store: &impl EventStore<A>,
    upto_index: usize,
    convert: F,
) -> Result<P, ProjectionError>
where
    P: Projector<Event = E>,
    A: Clone + Eq + std::hash::Hash,
    E: Send,
    F: Fn(&StoredEvent<A>) -> Result<E, ProjectionError>,
{
    let log = store.log();
    let end = log.len().min(upto_index);
    for ev in &log[..end] {
        projector.apply(&convert(ev)?).await?;
    }
    Ok(projector)
}

/// Convenience: the global-append index that corresponds to "just after" a given event.
///
/// Returns the 1-based count of events currently in the log, i.e. the `upto_index` that
/// reproduces the *current* state. Handy for capturing a checkpoint to later rewind to.
pub fn current_index<A: Clone + Eq + std::hash::Hash>(store: &impl EventStore<A>) -> usize {
    store.log().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::double_entry::{EntrySide, LedgerEntry, LedgerEvent, Transaction, TransactionId};
    use crate::event::Event;
    use crate::projection::BalanceProjection;
    use crate::store::InMemoryEventStore;
    use rust_decimal::Decimal;
    use tpt_erp_primitives::{Money, Usd};

    fn post(d: i64, c: i64) -> (TransactionId, crate::AccountId, LedgerEvent<Usd>) {
        let debit = crate::AccountId::new();
        let credit = crate::AccountId::new();
        let tx = Transaction {
            id: TransactionId::new(),
            entries: vec![
                LedgerEntry {
                    account: debit,
                    side: EntrySide::Debit,
                    amount: Money::new(Decimal::from(d)),
                },
                LedgerEntry {
                    account: credit,
                    side: EntrySide::Credit,
                    amount: Money::new(Decimal::from(c)),
                },
            ],
        };
        (tx.id, debit, LedgerEvent::TransactionPosted(tx))
    }

    fn push(store: &mut InMemoryEventStore<crate::AccountId>, ev: &LedgerEvent<Usd>) {
        let _ = store.append(Event::new(crate::AccountId::new(), "tx", ev).unwrap());
    }

    #[tokio::test]
    async fn replay_to_point_reproduces_historical_balances() {
        let mut store = InMemoryEventStore::<crate::AccountId>::default();
        // Three balanced transactions appended in order.
        let (_, d1, e1) = post(100, 100);
        let (_, d2, e2) = post(50, 50);
        let (_, d3, e3) = post(25, 25);
        push(&mut store, &e1);
        let after_first = current_index(&store); // 1
        push(&mut store, &e2);
        let after_second = current_index(&store); // 2
        push(&mut store, &e3);

        let convert = |se: &StoredEvent<crate::AccountId>| {
            let le = serde_json::from_value::<LedgerEvent<Usd>>(se.payload.clone())?;
            Ok::<_, ProjectionError>(le)
        };

        // As-of after the first transaction: only the first debit is visible (-100).
        let p1 = replay_point_in_time(BalanceProjection::<Usd>::default(), &store, after_first, &convert)
            .await
            .unwrap();
        assert_eq!(p1.balance_of(&d1).amount(), Decimal::from(-100));

        // As-of after the second: first two debits.
        let p2 = replay_point_in_time(BalanceProjection::<Usd>::default(), &store, after_second, &convert)
            .await
            .unwrap();
        assert_eq!(p2.balance_of(&d1).amount(), Decimal::from(-100));
        assert_eq!(p2.balance_of(&d2).amount(), Decimal::from(-50));

        // Current index replays everything.
        let p_all = replay_point_in_time(
            BalanceProjection::<Usd>::default(),
            &store,
            current_index(&store),
            &convert,
        )
        .await
        .unwrap();
        assert_eq!(p_all.balance_of(&d1).amount(), Decimal::from(-100));
        assert_eq!(p_all.balance_of(&d3).amount(), Decimal::from(-25));
    }

    #[tokio::test]
    async fn replay_to_point_clamps_past_end() {
        let mut store = InMemoryEventStore::<crate::AccountId>::default();
        let (_, _, e1) = post(10, 10);
        push(&mut store, &e1);
        let convert = |se: &StoredEvent<crate::AccountId>| {
            Ok::<_, ProjectionError>(serde_json::from_value::<LedgerEvent<Usd>>(se.payload.clone())?)
        };
        // Asking for an index beyond the log end replays all available events.
        let proj = replay_point_in_time(BalanceProjection::<Usd>::default(), &store, 999, &convert)
            .await
            .unwrap();
        assert_eq!(proj.balances.len(), 2);
    }
}
