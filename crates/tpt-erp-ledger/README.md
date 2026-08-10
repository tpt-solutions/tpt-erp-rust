# tpt-erp-ledger

> The financial and audit heart of TPT ERP: event-sourced double-entry
> accounting with a CQRS projection engine.

`tpt-erp-ledger` enforces that money is never silently lost and that read models
can always be rebuilt from the source of truth — the event log.

## Modules

| Module | Responsibility |
|--------|----------------|
| [`event`](src/event.rs) | The append-only event schema ([`StoredEvent`], [`Event`]) and optimistic-concurrency errors. |
| [`store`](src/store.rs) | An in-memory [`EventStore`] with per-aggregate sequence numbers. |
| [`double_entry`](src/double_entry.rs) | The [`DoubleEntry`] trait enforcing that transactions **balance**. |
| [`projection`](src/projection.rs) | The CQRS [`Projector`] trait, a [`replay`] helper, and an example [`BalanceProjection`]. |

A production deployment persists the **same** [`StoredEvent`] shape to Postgres
and runs the same projectors — only the storage backend changes.

## Double-entry core

Every [`Transaction`] is parameterized by a single [`Currency`], so a
cross-currency transaction is *unrepresentable*. The [`DoubleEntry`] trait checks
that total debits equal total credits before anything reaches the database:

```rust
use tpt_erp_ledger::{Transaction, DoubleEntry, LedgerEntry, EntrySide};
use tpt_erp_primitives::{Money, Usd, Id, Entity};

struct Account; impl Entity for Account {}
type AccountId = Id<Account>;

let tx = Transaction::<Usd> {
    id: Id::new(),
    entries: vec![
        LedgerEntry { account: AccountId::new(), side: EntrySide::Debit,  amount: Money::from_major(100) },
        LedgerEntry { account: AccountId::new(), side: EntrySide::Credit, amount: Money::from_major(100) },
    ],
};
assert!(tx.validate().is_ok()); // unbalanced txns fail validation
```

`validate()` returns `DoubleEntryError::Unbalanced` or `::Empty` (fewer than two
legs).

## Event sourcing

The [`EventStore`] appends immutably and stamps every event with a
monotonically increasing `sequence` **per aggregate**. Appending with
`append_versioned(expected)` is the optimistic-concurrency guard: a concurrent
writer that advanced the version first triggers an `EventStoreError::Conflict`.

```rust
use tpt_erp_ledger::{EventStore, Event};

let mut store: EventStore<AccountId> = EventStore::default();
store.append(Event::new(acc, "Created", &"x")?);
assert_eq!(store.version(&acc), 1);
```

## CQRS projections

Read models are built by folding events through a [`Projector`]. Because the log
is the source of truth, any read model can be rebuilt from scratch:

```rust
use tpt_erp_ledger::{BalanceProjection, replay, LedgerEvent};

let proj = replay(BalanceProjection::<Usd>::default(), events).await?;
let bal = proj.balance_of(&some_account);
```

The bundled [`BalanceProjection`] tracks every account's running balance by
replaying `LedgerEvent::TransactionPosted`. Rebuilding from scratch always
equals incremental application — there is no risk of drift.

## Status

Early development (0.1.0). The in-memory store, double-entry rules, and
projection engine are implemented and tested; a Postgres-backed store and richer
projections are planned. APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
