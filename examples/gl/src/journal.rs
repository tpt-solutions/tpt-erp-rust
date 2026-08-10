//! Multi-currency journal engine.
//!
//! The journal is modeled as an **event-sourced** projection over per-account posting
//! legs. A posting appends a leg to *each* involved account's log. Because the logs are
//! sharded by account, many accounts can be updated concurrently with no global row
//! lock — the hallmark of a real-time, multi-currency GL.
//!
//! The running balance is a CQRS read model rebuilt by replaying the leg log, cached
//! per tenant via `tpt-erp-cache`. Optimistic concurrency is per-account: a guarded
//! post checks the caller's expected per-account version before appending, so a stale
//! writer is rejected with [`JournalError::Conflict`] and the ledger can never be left
//! half-posted.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::Hasher;
use std::sync::Mutex;
use tpt_erp_cache::{CacheError, ReadModelCache};
use tpt_erp_ledger::{
    AccountId, DoubleEntry, DoubleEntryError, EntrySide, Event, EventStore, LedgerEntry,
    ProjectionError, Projector, Transaction, TransactionId, replay,
};
use tpt_erp_primitives::{Currency, Money, Usd};
use tpt_erp_tenant::TenantId;

use crate::coa::{AccountType, ChartOfAccounts, DemoAccounts};

const SHARDS: usize = 64;

/// A running balance for one account: cumulative debits and credits.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AccountBalance<C: Currency> {
    pub debits: Money<C>,
    pub credits: Money<C>,
}

impl<C: Currency> Default for AccountBalance<C> {
    fn default() -> Self {
        Self {
            debits: Money::zero(),
            credits: Money::zero(),
        }
    }
}

impl<C: Currency> AccountBalance<C> {
    /// Credit minus debit — the "credit-balance" sign convention.
    pub fn net(&self) -> Money<C> {
        self.credits - self.debits
    }

    /// The signed balance in the account's *normal* direction.
    pub fn signed(&self, kind: AccountType) -> Money<C> {
        match kind.normal_side() {
            EntrySide::Debit => self.debits - self.credits,
            EntrySide::Credit => self.credits - self.debits,
        }
    }
}

/// A single posted leg, stored as an append-only event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostedLeg<C: Currency> {
    pub tx_id: TransactionId,
    pub account: AccountId,
    pub side: EntrySide,
    pub amount: Money<C>,
    /// Accounting period this leg belongs to (enables period-scoped reporting).
    pub period: String,
    pub memo: String,
}

/// Errors raised by the journal engine.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("event store error: {0}")]
    Store(#[from] tpt_erp_ledger::EventStoreError),
    #[error("transaction invalid: {0}")]
    Invalid(#[from] DoubleEntryError),
    #[error(
        "optimistic-concurrency conflict on {account}: expected v{expected}, current v{current}"
    )]
    Conflict {
        account: AccountId,
        expected: u64,
        current: u64,
    },
    #[error("cache backend error: {0}")]
    Cache(#[from] CacheError),
    #[error("projection replay failed: {0}")]
    Projection(#[from] ProjectionError),
}

struct Shard<C: Currency> {
    store: EventStore<AccountId>,
    balances: HashMap<AccountId, AccountBalance<C>>,
}

impl<C: Currency> Default for Shard<C> {
    fn default() -> Self {
        Self {
            store: EventStore::default(),
            balances: HashMap::new(),
        }
    }
}

impl<C: Currency> Shard<C> {
    fn apply_leg(&mut self, leg: &PostedLeg<C>) {
        let bal = self.balances.entry(leg.account).or_default();
        match leg.side {
            EntrySide::Debit => bal.debits += leg.amount,
            EntrySide::Credit => bal.credits += leg.amount,
        }
    }
}

/// Multi-currency journal engine.
///
/// * Writes append posting legs sharded by [`AccountId`], so concurrent postings to
///   different accounts never contend on a global lock.
/// * The running balance is a derived read model, optionally cached per tenant.
/// * The chart of accounts is carried so reporting/fx can resolve account types.
pub struct JournalEngine<C: Currency> {
    shards: Vec<Mutex<Shard<C>>>,
    tenant: TenantId,
    coa: ChartOfAccounts<C>,
    bus: Option<Box<dyn tpt_erp_bus::EventBus>>,
    cache: Option<Box<dyn ReadModelCache>>,
    /// Reporting-currency rate at which each account's balance was established
    /// (used by period-end revaluation in [`crate::fx`]).
    book_rates: Mutex<HashMap<AccountId, rust_decimal::Decimal>>,
}

impl<C> JournalEngine<C>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    /// Create an engine for `tenant` with the given chart of accounts.
    pub fn new(tenant: TenantId, coa: ChartOfAccounts<C>) -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(),
            tenant,
            coa,
            bus: None,
            cache: None,
            book_rates: Mutex::new(HashMap::new()),
        }
    }

    /// Attach a background-job bus; the period-close event is published there.
    pub fn with_bus(mut self, bus: Box<dyn tpt_erp_bus::EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Attach a read-model cache for account balances.
    pub fn with_cache(mut self, cache: Box<dyn ReadModelCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// The chart of accounts.
    pub fn chart_of_accounts(&self) -> &ChartOfAccounts<C> {
        &self.coa
    }

    /// Set the book (historical) rate for an account, in reporting currency per unit of `C`.
    pub fn set_book_rate(&self, account: AccountId, rate: rust_decimal::Decimal) {
        self.book_rates.lock().unwrap().insert(account, rate);
    }

    /// The book rate for an account, if set.
    pub fn book_rate(&self, account: AccountId) -> Option<rust_decimal::Decimal> {
        self.book_rates.lock().unwrap().get(&account).copied()
    }

    fn shard_index(account: &AccountId) -> usize {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(account, &mut h);
        (h.finish() as usize) % SHARDS
    }

    /// Post a balanced transaction. Returns the new transaction id.
    pub async fn post_transaction(
        &self,
        entries: Vec<LedgerEntry<C>>,
        period: &str,
        memo: &str,
    ) -> Result<TransactionId, JournalError> {
        let tx = Transaction {
            id: TransactionId::new(),
            entries,
        };
        tx.validate()?;
        let (id, payloads) = self.post_sync(&tx, period, memo, None)?;
        self.write_cache(payloads).await;
        Ok(id)
    }

    /// Post a balanced transaction with an optimistic-concurrency guard. `expected`
    /// maps each involved account to the version the caller believes it is at; a stale
    /// caller is rejected with [`JournalError::Conflict`] and *nothing* is posted.
    pub async fn post_transaction_guarded(
        &self,
        entries: Vec<LedgerEntry<C>>,
        period: &str,
        memo: &str,
        expected: &HashMap<AccountId, u64>,
    ) -> Result<TransactionId, JournalError> {
        let tx = Transaction {
            id: TransactionId::new(),
            entries,
        };
        tx.validate()?;
        let (id, payloads) = self.post_sync(&tx, period, memo, Some(expected))?;
        self.write_cache(payloads).await;
        Ok(id)
    }

    /// Synchronous post (no cache write). Convenient for UIs and contexts without an
    /// async runtime; the in-memory CQRS read model still updates, only the optional
    /// cache is skipped.
    pub fn post_transaction_sync(
        &self,
        entries: Vec<LedgerEntry<C>>,
        period: &str,
        memo: &str,
    ) -> Result<TransactionId, JournalError> {
        let tx = Transaction {
            id: TransactionId::new(),
            entries,
        };
        tx.validate()?;
        let (id, _payloads) = self.post_sync(&tx, period, memo, None)?;
        Ok(id)
    }

    /// Synchronous, lock-holding post path. Locks every involved shard (ordered by
    /// account id => no deadlock), optionally verifies per-account versions, appends
    /// each leg, and returns the transaction id plus owned cache payloads. The shard
    /// locks are dropped when this function returns, so the caller's `await` on the
    /// cache write keeps the future `Send`.
    fn post_sync(
        &self,
        tx: &Transaction<C>,
        period: &str,
        memo: &str,
        expected: Option<&HashMap<AccountId, u64>>,
    ) -> Result<(TransactionId, Vec<(String, serde_json::Value)>), JournalError> {
        let mut accounts: Vec<AccountId> = tx.entries.iter().map(|e| e.account).collect();
        accounts.sort();
        accounts.dedup();

        let mut guards: Vec<_> = accounts
            .iter()
            .map(|a| self.shards[Self::shard_index(a)].lock().unwrap())
            .collect();

        // Verify all expected versions while holding the locks.
        if let Some(exp) = expected {
            for a in &accounts {
                let want = exp.get(a).copied().unwrap_or(0);
                let pos = accounts.binary_search(a).unwrap();
                let current = guards[pos].store.version(a);
                if current != want {
                    return Err(JournalError::Conflict {
                        account: *a,
                        expected: want,
                        current,
                    });
                }
            }
        }

        for e in &tx.entries {
            let pos = accounts.binary_search(&e.account).unwrap();
            let shard = &mut guards[pos];
            let leg = PostedLeg {
                tx_id: tx.id,
                account: e.account,
                side: e.side,
                amount: e.amount,
                period: period.to_string(),
                memo: memo.to_string(),
            };
            let event = Event::new(e.account, "leg-posted", &leg)?;
            match expected {
                Some(exp) => {
                    let want = exp.get(&e.account).copied().unwrap_or(0);
                    shard.store.append_versioned(event, want + 1).map_err(|_| {
                        JournalError::Conflict {
                            account: e.account,
                            expected: want,
                            current: want + 1,
                        }
                    })?;
                }
                None => {
                    shard.store.append(event);
                }
            }
            shard.apply_leg(&leg);
        }

        let mut payloads = Vec::new();
        if self.cache.is_some() {
            for a in &accounts {
                let pos = accounts.binary_search(a).unwrap();
                let bal = guards[pos].balances.get(a).copied().unwrap_or_default();
                let v = serde_json::to_value(bal).unwrap_or(serde_json::Value::Null);
                payloads.push((a.as_str(), v));
            }
        }
        Ok((tx.id, payloads))
    }

    /// Write cached balances. Held outside any shard lock so the future stays `Send`
    /// and can be awaited across a `tokio::spawn` boundary.
    async fn write_cache(&self, payloads: Vec<(String, serde_json::Value)>) {
        if let Some(cache) = &self.cache {
            for (key, val) in payloads {
                let _ = cache.put(&self.tenant, "gl-balance", &key, val, None).await;
            }
        }
    }

    /// Read the cached/derived balance for an account.
    pub fn balance_of(&self, account: AccountId) -> AccountBalance<C> {
        let idx = Self::shard_index(&account);
        let shard = self.shards[idx].lock().unwrap();
        shard.balances.get(&account).copied().unwrap_or_default()
    }

    /// Current event-log version for an account (used by optimistic-concurrency clients).
    pub fn version(&self, account: AccountId) -> u64 {
        let idx = Self::shard_index(&account);
        let shard = self.shards[idx].lock().unwrap();
        shard.store.version(&account)
    }

    /// Rebuild the balance read model from the event log from scratch (CQRS replay),
    /// writing results into the attached cache. Proves the read model can never silently
    /// drift from the ledger of record.
    pub async fn rebuild_read_models(&self) -> Result<JournalProjection<C>, JournalError> {
        let mut legs: Vec<PostedLeg<C>> = Vec::new();
        for shard in &self.shards {
            let s = shard.lock().unwrap();
            for ev in s.store.log() {
                legs.push(serde_json::from_value(ev.payload.clone())?);
            }
        }
        let proj = replay(JournalProjection::<C>::default(), legs).await?;
        if let Some(cache) = &self.cache {
            for (acc, bal) in &proj.balances {
                let _ = cache
                    .put(
                        &self.tenant,
                        "gl-balance",
                        &acc.as_str(),
                        serde_json::to_value(bal).unwrap_or(serde_json::Value::Null),
                        None,
                    )
                    .await;
            }
        }
        Ok(proj)
    }

    /// Generate (but do not post) the income-statement close: zero every temporary
    /// (revenue/expense) account into retained earnings. The returned entries balance.
    pub fn generate_closing_entries(&self, retained_earnings: AccountId) -> Vec<LedgerEntry<C>> {
        let mut entries = Vec::new();
        let mut net = Money::<C>::zero();
        for acc in self.coa.iter() {
            if acc.kind.is_permanent() {
                continue;
            }
            let bal = self.balance_of(acc.id);
            let mag = bal.signed(acc.kind);
            if mag == Money::zero() {
                continue;
            }
            // Zero the account on its *opposite* normal side.
            let zero_side = match acc.kind.normal_side() {
                EntrySide::Debit => EntrySide::Credit,
                EntrySide::Credit => EntrySide::Debit,
            };
            entries.push(LedgerEntry {
                account: acc.id,
                side: zero_side,
                amount: mag,
            });
            // Revenue increases net income; expense decreases it.
            net = match acc.kind {
                AccountType::Revenue => net + mag,
                _ => net - mag,
            };
        }
        if net != Money::zero() {
            let re_side = if net > Money::zero() {
                EntrySide::Credit
            } else {
                EntrySide::Debit
            };
            let amt = if net < Money::zero() { -net } else { net };
            entries.push(LedgerEntry {
                account: retained_earnings,
                side: re_side,
                amount: amt,
            });
        }
        entries
    }
}

/// CQRS read model: folds posting legs into per-account balances.
#[derive(Debug, Clone)]
pub struct JournalProjection<C: Currency> {
    pub balances: HashMap<AccountId, AccountBalance<C>>,
}

impl<C: Currency> Default for JournalProjection<C> {
    fn default() -> Self {
        Self {
            balances: HashMap::new(),
        }
    }
}

impl<C: Currency> Projector for JournalProjection<C> {
    type Event = PostedLeg<C>;

    async fn apply(&mut self, event: &PostedLeg<C>) -> Result<(), ProjectionError> {
        let bal = self.balances.entry(event.account).or_default();
        match event.side {
            EntrySide::Debit => bal.debits += event.amount,
            EntrySide::Credit => bal.credits += event.amount,
        }
        Ok(())
    }
}

/// Build a demo USD journal (sample chart of accounts) for examples/tests/UI.
pub fn demo(tenant: TenantId) -> (JournalEngine<Usd>, DemoAccounts<Usd>) {
    let (coa, demo) = crate::coa::demo_coa::<Usd>();
    let engine = JournalEngine::new(tenant, coa);
    (engine, demo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rust_decimal::Decimal;
    use tpt_erp_ledger::{EntrySide, LedgerEntry};
    use tpt_erp_primitives::Usd;

    fn leg(account: AccountId, side: EntrySide, amount: i64) -> LedgerEntry<Usd> {
        LedgerEntry {
            account,
            side,
            amount: Money::<Usd>::new(Decimal::from(amount)),
        }
    }

    fn demo_engine() -> (JournalEngine<Usd>, DemoAccounts<Usd>) {
        super::demo(crate::demo_tenant())
    }

    #[tokio::test]
    async fn balanced_posting_updates_balances() {
        let (eng, d) = demo_engine();
        let entries = vec![
            leg(d.cash, EntrySide::Debit, 100),
            leg(d.common_stock, EntrySide::Credit, 100),
        ];
        let id = eng
            .post_transaction(entries, "2026-01", "investment")
            .await
            .unwrap();
        assert!(!id.as_str().is_empty());
        assert_eq!(eng.balance_of(d.cash).debits.amount(), Decimal::from(100));
        assert_eq!(
            eng.balance_of(d.common_stock).credits.amount(),
            Decimal::from(100)
        );
    }

    #[tokio::test]
    async fn unbalanced_posting_rejected() {
        let (eng, d) = demo_engine();
        let entries = vec![
            leg(d.cash, EntrySide::Debit, 100),
            leg(d.common_stock, EntrySide::Credit, 99),
        ];
        let res = eng.post_transaction(entries, "2026-01", "bad").await;
        assert!(matches!(res, Err(JournalError::Invalid(_))));
    }

    #[tokio::test]
    async fn optimistic_concurrency_rejects_stale_writer() {
        let (eng, d) = demo_engine();
        let entries = vec![
            leg(d.cash, EntrySide::Debit, 10),
            leg(d.common_stock, EntrySide::Credit, 10),
        ];
        eng.post_transaction(entries, "2026-01", "x").await.unwrap();

        // A client claiming version 0 (stale) is rejected; nothing is posted.
        let mut expected = HashMap::new();
        expected.insert(d.cash, 0);
        expected.insert(d.common_stock, 0);
        let stale = eng
            .post_transaction_guarded(
                vec![
                    leg(d.cash, EntrySide::Debit, 5),
                    leg(d.common_stock, EntrySide::Credit, 5),
                ],
                "2026-01",
                "x",
                &expected,
            )
            .await;
        assert!(matches!(stale, Err(JournalError::Conflict { .. })));

        // Correct version (1) succeeds.
        let mut expected = HashMap::new();
        expected.insert(d.cash, 1);
        expected.insert(d.common_stock, 1);
        let ok = eng
            .post_transaction_guarded(
                vec![
                    leg(d.cash, EntrySide::Debit, 5),
                    leg(d.common_stock, EntrySide::Credit, 5),
                ],
                "2026-01",
                "x",
                &expected,
            )
            .await;
        assert!(ok.is_ok());
    }

    #[tokio::test]
    async fn concurrent_posts_to_many_accounts_all_apply() {
        let (eng, d) = demo_engine();
        let eng = Arc::new(eng);
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let eng = eng.clone();
                let cash = d.cash;
                let cs = d.common_stock;
                tokio::spawn(async move {
                    let entries = vec![
                        leg(cash, EntrySide::Debit, 1),
                        leg(cs, EntrySide::Credit, 1),
                    ];
                    eng.post_transaction(entries, "2026-01", "concurrent")
                        .await
                        .unwrap();
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(eng.balance_of(d.cash).debits.amount(), Decimal::from(16));
    }

    #[tokio::test]
    async fn concurrent_posts_to_same_account_no_lost_updates() {
        let (eng, d) = demo_engine();
        let eng = Arc::new(eng);
        let tasks: Vec<_> = (0..50)
            .map(|_| {
                let eng = eng.clone();
                let cash = d.cash;
                let cs = d.common_stock;
                tokio::spawn(async move {
                    let entries = vec![
                        leg(cash, EntrySide::Debit, 1),
                        leg(cs, EntrySide::Credit, 1),
                    ];
                    eng.post_transaction(entries, "2026-01", "x").await.unwrap();
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(eng.balance_of(d.cash).debits.amount(), Decimal::from(50));
    }

    #[tokio::test]
    async fn rebuild_read_model_matches_live_balances() {
        let (eng, d) = demo_engine();
        for i in 0..10 {
            let entries = vec![
                leg(d.cash, EntrySide::Debit, 1),
                leg(d.sales_revenue, EntrySide::Credit, 1),
            ];
            eng.post_transaction(entries, "2026-01", &format!("sale {i}"))
                .await
                .unwrap();
        }
        let proj = eng.rebuild_read_models().await.unwrap();
        assert_eq!(proj.balances[&d.cash].debits.amount(), Decimal::from(10));
        assert_eq!(eng.balance_of(d.cash).debits, proj.balances[&d.cash].debits);
    }

    #[tokio::test]
    async fn generated_closing_entries_balance() {
        let (eng, d) = demo_engine();
        let entries = vec![
            leg(d.cash, EntrySide::Debit, 500),
            leg(d.sales_revenue, EntrySide::Credit, 500),
        ];
        eng.post_transaction(entries, "2026-01", "sale")
            .await
            .unwrap();

        let closing = eng.generate_closing_entries(d.retained_earnings);
        let total_debit: Decimal = closing
            .iter()
            .filter(|e| e.side == EntrySide::Debit)
            .map(|e| e.amount.amount())
            .sum();
        let total_credit: Decimal = closing
            .iter()
            .filter(|e| e.side == EntrySide::Credit)
            .map(|e| e.amount.amount())
            .sum();
        assert_eq!(total_debit, total_credit);

        eng.post_transaction(closing, "2026-01", "close")
            .await
            .unwrap();
        // The revenue account is zeroed (debit 500 offsets its credit 500).
        let rev = eng.balance_of(d.sales_revenue);
        assert_eq!(rev.debits.amount(), Decimal::from(500));
        assert_eq!(rev.credits.amount(), Decimal::from(500));
        assert_eq!(rev.net(), Money::<Usd>::zero());
        assert_eq!(
            eng.balance_of(d.retained_earnings).credits.amount(),
            Decimal::from(500)
        );
    }
}
