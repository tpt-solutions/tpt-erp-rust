//! Multi-entity consolidation with inter-company eliminations.
//!
//! Each legal entity keeps its own [`JournalEngine`] (its own chart of accounts) in the
//! group's *reporting* currency `C` (entities in a foreign currency are revalued into `C`
//! first via [`crate::fx`]). Consolidation then folds every subsidiary's trial balance into
//! a single parent [`ConsolidatedTrialBalance`], preserving per-entity attribution so the
//! inter-company (due-to / due-from) balances can be identified and removed.
//!
//! Elimination is **deterministic**: it is driven by an explicit list of
//! [`EliminationPair`]s (a subsidiary's `Due-From` asset offsetting another subsidiary's
//! `Due-To` liability) *or* by a tag convention where inter-company accounts are tagged
//! with their counterparty entity (see [`derive_elimination_pairs`]). Either way the
//! elimination never invents numbers — it nets out exactly the matched balances.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tpt_erp_ledger::{AccountId, EntrySide, LedgerEntry};
use tpt_erp_primitives::{Currency, Entity, Id, Money};

use crate::coa::AccountType;
use crate::journal::JournalEngine;

/// Marker entity kind for a legal entity (a subsidiary in a consolidated group).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LegalEntity;

impl Entity for LegalEntity {}

/// Strong id of a legal entity (subsidiary) in the consolidation group.
pub type EntityId = Id<LegalEntity>;

/// One account's debit/credit totals for a single entity, reporting currency `C`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EntityBalance<C: Currency> {
    pub account: AccountId,
    pub kind: AccountType,
    pub debits: Money<C>,
    pub credits: Money<C>,
}

impl<C: Currency> EntityBalance<C> {
    /// The signed (normal-direction) balance of this account.
    pub fn signed(&self) -> Money<C> {
        match self.kind.normal_side() {
            EntrySide::Debit => self.debits - self.credits,
            EntrySide::Credit => self.credits - self.debits,
        }
    }
}

/// A trial balance for one legal entity, tagged with its [`EntityId`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityTrialBalance<C: Currency> {
    pub entity: EntityId,
    pub rows: Vec<EntityBalance<C>>,
}

impl<C: Currency> EntityTrialBalance<C> {
    /// Whether the entity's own books balance (debits == credits).
    pub fn is_balanced(&self) -> bool {
        let d: Money<C> = self
            .rows
            .iter()
            .map(|r| r.debits)
            .fold(Money::zero(), |a, b| a + b);
        let c: Money<C> = self
            .rows
            .iter()
            .map(|r| r.credits)
            .fold(Money::zero(), |a, b| a + b);
        d == c
    }
}

/// Build an entity's trial balance directly from its live journal engine balances.
pub fn entity_trial_balance<C>(engine: &JournalEngine<C>, entity: EntityId) -> EntityTrialBalance<C>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    let mut rows = Vec::new();
    for acc in engine.chart_of_accounts().iter() {
        let bal = engine.balance_of(acc.id);
        rows.push(EntityBalance {
            account: acc.id,
            kind: acc.kind,
            debits: bal.debits,
            credits: bal.credits,
        });
    }
    EntityTrialBalance { entity, rows }
}

/// A single (entity, account) balance in the pre-elimination consolidated view.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConsolidatedRow<C: Currency> {
    pub entity: EntityId,
    pub account: AccountId,
    pub kind: AccountType,
    pub debits: Money<C>,
    pub credits: Money<C>,
}

impl<C: Currency> ConsolidatedRow<C> {
    /// The signed (normal-direction) balance of this consolidated line.
    pub fn signed(&self) -> Money<C> {
        match self.kind.normal_side() {
            EntrySide::Debit => self.debits - self.credits,
            EntrySide::Credit => self.credits - self.debits,
        }
    }
}

/// The parent (group) trial balance before eliminations: every subsidiary's balances,
/// carrying their entity attribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedTrialBalance<C: Currency> {
    pub rows: Vec<ConsolidatedRow<C>>,
    pub total_debits: Money<C>,
    pub total_credits: Money<C>,
}

impl<C: Currency> ConsolidatedTrialBalance<C> {
    /// True iff the consolidated books balance (debits == credits).
    pub fn is_balanced(&self) -> bool {
        self.total_debits == self.total_credits
    }

    /// Look up a single (entity, account) consolidated row, if present.
    pub fn find(&self, entity: EntityId, account: AccountId) -> Option<&ConsolidatedRow<C>> {
        self.rows
            .iter()
            .find(|r| r.entity == entity && r.account == account)
    }
}

/// Combine subsidiary trial balances into a parent consolidated trial balance.
///
/// Balances are unioned (each entity's accounts keep their own attribution), not cross-summed,
/// because each subsidiary holds its own chart of accounts. The parent totals are simply the
/// sum of all subsidiary debits/credits, so a balanced group of balanced books remains balanced.
pub fn consolidate<C: Currency>(inputs: &[EntityTrialBalance<C>]) -> ConsolidatedTrialBalance<C> {
    let mut rows = Vec::new();
    let mut total_debits = Money::<C>::zero();
    let mut total_credits = Money::<C>::zero();

    for input in inputs {
        for r in &input.rows {
            total_debits += r.debits;
            total_credits += r.credits;
            rows.push(ConsolidatedRow {
                entity: input.entity,
                account: r.account,
                kind: r.kind,
                debits: r.debits,
                credits: r.credits,
            });
        }
    }

    // Deterministic ordering: by entity then account id, so reports are stable.
    rows.sort_by(|a, b| {
        a.entity
            .as_str()
            .cmp(&b.entity.as_str())
            .then_with(|| a.account.as_str().cmp(&b.account.as_str()))
    });

    ConsolidatedTrialBalance {
        rows,
        total_debits,
        total_credits,
    }
}

/// A directive to eliminate an inter-entity balance.
///
/// `from_entity`'s `from_account` is an inter-company **asset** (a `Due-From`, normally a
/// debit) that offsets `to_entity`'s `to_account`, an inter-company **liability** (a
/// `Due-To`, normally a credit) of the same amount. Elimination removes both legs so the
/// group does not double-count the internal receivable/payable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EliminationPair {
    pub from_entity: EntityId,
    pub from_account: AccountId,
    pub to_entity: EntityId,
    pub to_account: AccountId,
}

/// A consolidation journal entry to post against the parent ledger: it removes the
/// inter-company balances (credit the `Due-From`, debit the `Due-To`).
#[derive(Debug, Clone, Copy)]
pub struct EliminationEntry<C: Currency> {
    pub account: AccountId,
    pub side: EntrySide,
    pub amount: Money<C>,
}

/// The outcome of running eliminations over a consolidated trial balance.
#[derive(Debug, Clone)]
pub struct EliminationResult<C: Currency> {
    /// The consolidated trial balance after the matched inter-company balances are removed.
    pub trial_balance: ConsolidatedTrialBalance<C>,
    /// The entries to post to the parent ledger to effect the eliminations.
    pub entries: Vec<EliminationEntry<C>>,
    /// The total amount of inter-company balance eliminated from the group.
    pub total_eliminated: Money<C>,
}

/// Errors raised by the consolidation/elimination routines.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConsolidationError {
    #[error("elimination pair references unknown account {account} in entity {entity}")]
    UnknownAccount {
        entity: EntityId,
        account: AccountId,
    },
}

/// Eliminate inter-entity balances from a consolidated trial balance using an explicit list
/// of [`EliminationPair`]s.
///
/// For each pair the overlapping (positive) balance is `min(signed_from, signed_to)`; that
/// amount is removed from the `Due-From` (its debit total) and from the `Due-To` (its credit
/// total), and a pair of parent-ledger entries is recorded. The consolidated books remain
/// balanced because the two legs are reduced by the same amount.
///
/// # Errors
/// Returns [`ConsolidationError::UnknownAccount`] if a pair names an account/entity that is
/// not present in the consolidated trial balance.
pub fn eliminate<C: Currency>(
    ctb: &ConsolidatedTrialBalance<C>,
    pairs: &[EliminationPair],
) -> Result<EliminationResult<C>, ConsolidationError> {
    let mut rows = ctb.rows.clone();
    let mut entries = Vec::new();
    let mut total_eliminated = Money::<C>::zero();

    for pair in pairs {
        let from_idx = rows
            .iter()
            .position(|r| r.entity == pair.from_entity && r.account == pair.from_account);
        let to_idx = rows
            .iter()
            .position(|r| r.entity == pair.to_entity && r.account == pair.to_account);

        let from_idx = from_idx.ok_or(ConsolidationError::UnknownAccount {
            entity: pair.from_entity,
            account: pair.from_account,
        })?;
        let to_idx = to_idx.ok_or(ConsolidationError::UnknownAccount {
            entity: pair.to_entity,
            account: pair.to_account,
        })?;

        let signed_from = rows[from_idx].signed();
        let signed_to = rows[to_idx].signed();
        let overlap = signed_from.min(signed_to);
        // Only positive balances can be eliminated; a negative (contra) balance contributes
        // nothing to the inter-company receivable/payable.
        let amount = if overlap > Money::zero() {
            overlap
        } else {
            Money::zero()
        };

        if amount > Money::zero() {
            rows[from_idx].debits -= amount;
            rows[to_idx].credits -= amount;

            // Parent-ledger entries: credit the Due-From (remove the asset), debit the Due-To
            // (remove the liability). These exactly offset each other.
            entries.push(EliminationEntry {
                account: pair.from_account,
                side: EntrySide::Credit,
                amount,
            });
            entries.push(EliminationEntry {
                account: pair.to_account,
                side: EntrySide::Debit,
                amount,
            });
            total_eliminated += amount;
        }
    }

    let mut total_debits = Money::<C>::zero();
    let mut total_credits = Money::<C>::zero();
    for r in &rows {
        total_debits += r.debits;
        total_credits += r.credits;
    }

    Ok(EliminationResult {
        trial_balance: ConsolidatedTrialBalance {
            rows,
            total_debits,
            total_credits,
        },
        entries,
        total_eliminated,
    })
}

/// Derive elimination pairs from an inter-company **tag convention**.
///
/// `tags` maps an account id to the [`EntityId`] of its counterparty. The direction is taken
/// from the account's normal balance: an `Asset`/`Expense` account is a `Due-From` (the
/// `from` side) and a `Liability`/`Equity` account is a `Due-To` (the `to` side). For every
/// `Due-From` of entity `E` tagged counterparty `X`, the routine pairs it with the `Due-To`
/// of entity `X` tagged counterparty `E`. The result is deterministic (sorted by entity then
/// account id) and needs no manual pair list — but only ever nets explicitly tagged balances.
pub fn derive_elimination_pairs<C: Currency>(
    ctb: &ConsolidatedTrialBalance<C>,
    tags: &HashMap<AccountId, EntityId>,
) -> Vec<EliminationPair> {
    // (entity, counterparty) -> the Due-To row living in `entity` owed to `counterparty`.
    let mut due_to: HashMap<(EntityId, EntityId), ConsolidatedRow<C>> = HashMap::new();
    let mut due_from: Vec<ConsolidatedRow<C>> = Vec::new();

    for row in &ctb.rows {
        let Some(&counterparty) = tags.get(&row.account) else {
            continue;
        };
        match row.kind {
            AccountType::Asset | AccountType::Expense => due_from.push(*row),
            AccountType::Liability | AccountType::Equity => {
                due_to.insert((row.entity, counterparty), *row);
            }
            AccountType::Revenue => {}
        }
    }

    let mut pairs = Vec::new();
    for from in due_from {
        let counterparty = tags[&from.account];
        if let Some(&to) = due_to.get(&(counterparty, from.entity)) {
            pairs.push(EliminationPair {
                from_entity: from.entity,
                from_account: from.account,
                to_entity: to.entity,
                to_account: to.account,
            });
        }
    }

    pairs.sort_by(|a, b| {
        a.from_entity
            .as_str()
            .cmp(&b.from_entity.as_str())
            .then_with(|| a.from_account.as_str().cmp(&b.from_account.as_str()))
            .then_with(|| a.to_entity.as_str().cmp(&b.to_entity.as_str()))
            .then_with(|| a.to_account.as_str().cmp(&b.to_account.as_str()))
    });
    pairs
}

/// Convenience: convert elimination entries into balanced parent-ledger [`LedgerEntry`]s.
pub fn elimination_ledger_entries<C: Currency>(
    entries: &[EliminationEntry<C>],
) -> Vec<LedgerEntry<C>> {
    entries
        .iter()
        .map(|e| LedgerEntry {
            account: e.account,
            side: e.side,
            amount: e.amount,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coa::{Account, AccountType, DemoAccounts};
    use rust_decimal::Decimal;
    use tpt_erp_ledger::{EntrySide, LedgerEntry};
    use tpt_erp_primitives::Usd;

    fn money(i: i64) -> Money<Usd> {
        Money::<Usd>::new(Decimal::from(i))
    }

    fn leg(account: AccountId, side: EntrySide, amount: i64) -> LedgerEntry<Usd> {
        LedgerEntry {
            account,
            side,
            amount: money(amount),
        }
    }

    fn entity_id() -> EntityId {
        EntityId::new()
    }

    /// Build a USD entity ledger whose chart already contains an optional inter-company
    /// `Due-From` and `Due-To` account, and post a simple balanced setup.
    async fn make_entity(
        due_from: Option<&str>,
        due_to: Option<&str>,
    ) -> (
        JournalEngine<Usd>,
        DemoAccounts<Usd>,
        Option<AccountId>,
        Option<AccountId>,
    ) {
        let (mut coa, demo) = crate::coa::demo_coa::<Usd>();
        let due_from_id =
            due_from.map(|code| coa.add(Account::new(code, "Due From", AccountType::Asset)));
        let due_to_id =
            due_to.map(|code| coa.add(Account::new(code, "Due To", AccountType::Liability)));
        let eng = JournalEngine::new(crate::demo_tenant(), coa);
        (eng, demo, due_from_id, due_to_id)
    }

    #[tokio::test]
    async fn consolidation_sums_subsidiary_balances() {
        let a = entity_id();
        let b = entity_id();

        let (eng_a, da, due_from_b, _) = make_entity(Some("1600"), None).await;
        let (eng_b, db, _, due_to_a) = make_entity(None, Some("2100")).await;
        let (due_from_b, due_to_a) = (due_from_b.unwrap(), due_to_a.unwrap());

        // A: invest 1000 cash, owe 200 to B (Due-From B, an asset on A's books).
        eng_a
            .post_transaction(
                vec![
                    leg(da.cash, EntrySide::Debit, 1000),
                    leg(da.common_stock, EntrySide::Credit, 1000),
                    leg(due_from_b, EntrySide::Debit, 200),
                    leg(da.accounts_payable, EntrySide::Credit, 200),
                ],
                "2026-01",
                "A setup",
            )
            .await
            .unwrap();

        // B: owes 200 to A (Due-To A, a liability on B's books), plus equity 800.
        eng_b
            .post_transaction(
                vec![
                    leg(db.cash, EntrySide::Debit, 800),
                    leg(db.common_stock, EntrySide::Credit, 800),
                    leg(due_to_a, EntrySide::Credit, 200),
                    leg(db.accounts_receivable, EntrySide::Debit, 200),
                ],
                "2026-01",
                "B setup",
            )
            .await
            .unwrap();

        let tb_a = entity_trial_balance(&eng_a, a);
        let tb_b = entity_trial_balance(&eng_b, b);
        assert!(tb_a.is_balanced());
        assert!(tb_b.is_balanced());

        let ctb = consolidate(&[tb_a, tb_b]);
        // Both subsidiaries balanced => group balanced.
        assert!(ctb.is_balanced());
        // Group cash is 1000 + 800 = 1800.
        let cash_debits: Decimal = ctb
            .rows
            .iter()
            .filter(|r| r.account == da.cash || r.account == db.cash)
            .map(|r| r.debits.amount())
            .sum();
        assert_eq!(cash_debits, Decimal::from(1800));
        // Inter-company balances present pre-elimination.
        assert_eq!(
            ctb.find(a, due_from_b).unwrap().debits.amount(),
            Decimal::from(200)
        );
        assert_eq!(
            ctb.find(b, due_to_a).unwrap().credits.amount(),
            Decimal::from(200)
        );
    }

    #[tokio::test]
    async fn eliminations_net_out_inter_entity_balances() {
        let a = entity_id();
        let b = entity_id();
        let (eng_a, da, due_from_b, _) = make_entity(Some("1600"), None).await;
        let (eng_b, db, _, due_to_a) = make_entity(None, Some("2100")).await;
        let (due_from_b, due_to_a) = (due_from_b.unwrap(), due_to_a.unwrap());

        eng_a
            .post_transaction(
                vec![
                    leg(da.cash, EntrySide::Debit, 1000),
                    leg(da.common_stock, EntrySide::Credit, 1000),
                    leg(due_from_b, EntrySide::Debit, 200),
                    leg(da.accounts_payable, EntrySide::Credit, 200),
                ],
                "2026-01",
                "A",
            )
            .await
            .unwrap();
        eng_b
            .post_transaction(
                vec![
                    leg(db.cash, EntrySide::Debit, 800),
                    leg(db.common_stock, EntrySide::Credit, 800),
                    leg(due_to_a, EntrySide::Credit, 200),
                    leg(db.accounts_receivable, EntrySide::Debit, 200),
                ],
                "2026-01",
                "B",
            )
            .await
            .unwrap();

        let ctb = consolidate(&[
            entity_trial_balance(&eng_a, a),
            entity_trial_balance(&eng_b, b),
        ]);

        let pair = EliminationPair {
            from_entity: a,
            from_account: due_from_b,
            to_entity: b,
            to_account: due_to_a,
        };
        let result = eliminate(&ctb, &[pair]).unwrap();

        // The matched inter-company balances are fully removed.
        assert_eq!(result.total_eliminated.amount(), Decimal::from(200));
        assert_eq!(
            result.trial_balance.find(a, due_from_b).unwrap().debits,
            Money::zero()
        );
        assert_eq!(
            result.trial_balance.find(b, due_to_a).unwrap().credits,
            Money::zero()
        );
        // Elimination entries are balanced and reference the correct accounts.
        let les = elimination_ledger_entries(&result.entries);
        assert_eq!(les.len(), 2);
        let total: Decimal = les.iter().map(|e| e.amount.amount()).sum();
        assert_eq!(total, Decimal::from(400)); // one debit + one credit of 200
        // Group still balances after elimination.
        assert!(result.trial_balance.is_balanced());
    }

    #[tokio::test]
    async fn partial_elimination_keeps_residual() {
        let a = entity_id();
        let b = entity_id();
        let (eng_a, da, due_from_b, _) = make_entity(Some("1600"), None).await;
        let (eng_b, db, _, due_to_a) = make_entity(None, Some("2100")).await;
        let (due_from_b, due_to_a) = (due_from_b.unwrap(), due_to_a.unwrap());

        // A is owed 300 by B, but B only records 100 owed to A (a real mismatch).
        eng_a
            .post_transaction(
                vec![
                    leg(da.cash, EntrySide::Debit, 1000),
                    leg(da.common_stock, EntrySide::Credit, 1000),
                    leg(due_from_b, EntrySide::Debit, 300),
                    leg(da.accounts_payable, EntrySide::Credit, 300),
                ],
                "2026-01",
                "A",
            )
            .await
            .unwrap();
        eng_b
            .post_transaction(
                vec![
                    leg(db.cash, EntrySide::Debit, 800),
                    leg(db.common_stock, EntrySide::Credit, 800),
                    leg(due_to_a, EntrySide::Credit, 100),
                    leg(db.accounts_receivable, EntrySide::Debit, 100),
                ],
                "2026-01",
                "B",
            )
            .await
            .unwrap();

        let ctb = consolidate(&[
            entity_trial_balance(&eng_a, a),
            entity_trial_balance(&eng_b, b),
        ]);
        let pair = EliminationPair {
            from_entity: a,
            from_account: due_from_b,
            to_entity: b,
            to_account: due_to_a,
        };
        let result = eliminate(&ctb, &[pair]).unwrap();

        // Only the overlapping 100 is eliminated; A keeps a 200 net receivable.
        assert_eq!(result.total_eliminated.amount(), Decimal::from(100));
        assert_eq!(
            result
                .trial_balance
                .find(a, due_from_b)
                .unwrap()
                .debits
                .amount(),
            Decimal::from(200)
        );
        assert_eq!(
            result.trial_balance.find(b, due_to_a).unwrap().credits,
            Money::zero()
        );
        assert!(result.trial_balance.is_balanced());
    }

    #[tokio::test]
    async fn tag_convention_derives_pairs_and_eliminates() {
        let a = entity_id();
        let b = entity_id();
        let (eng_a, da, due_from_b, _) = make_entity(Some("1600"), None).await;
        let (eng_b, db, _, due_to_a) = make_entity(None, Some("2100")).await;
        let (due_from_b, due_to_a) = (due_from_b.unwrap(), due_to_a.unwrap());

        eng_a
            .post_transaction(
                vec![
                    leg(da.cash, EntrySide::Debit, 1000),
                    leg(da.common_stock, EntrySide::Credit, 1000),
                    leg(due_from_b, EntrySide::Debit, 200),
                    leg(da.accounts_payable, EntrySide::Credit, 200),
                ],
                "2026-01",
                "A",
            )
            .await
            .unwrap();
        eng_b
            .post_transaction(
                vec![
                    leg(db.cash, EntrySide::Debit, 800),
                    leg(db.common_stock, EntrySide::Credit, 800),
                    leg(due_to_a, EntrySide::Credit, 200),
                    leg(db.accounts_receivable, EntrySide::Debit, 200),
                ],
                "2026-01",
                "B",
            )
            .await
            .unwrap();

        let ctb = consolidate(&[
            entity_trial_balance(&eng_a, a),
            entity_trial_balance(&eng_b, b),
        ]);

        // Convention: each inter-company account is tagged with its counterparty entity.
        let mut tags = HashMap::new();
        tags.insert(due_from_b, b);
        tags.insert(due_to_a, a);

        let pairs = derive_elimination_pairs(&ctb, &tags);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].from_entity, a);
        assert_eq!(pairs[0].from_account, due_from_b);
        assert_eq!(pairs[0].to_entity, b);
        assert_eq!(pairs[0].to_account, due_to_a);

        let result = eliminate(&ctb, &pairs).unwrap();
        assert_eq!(result.total_eliminated.amount(), Decimal::from(200));
    }

    #[tokio::test]
    async fn unknown_account_pair_errors() {
        let a = entity_id();
        let b = entity_id();
        let (eng_a, _, _, _) = make_entity(None, None).await;
        let (eng_b, db, _, _) = make_entity(None, None).await;
        let ctb = consolidate(&[
            entity_trial_balance(&eng_a, a),
            entity_trial_balance(&eng_b, b),
        ]);
        let pair = EliminationPair {
            from_entity: a,
            from_account: AccountId::new(),
            to_entity: b,
            to_account: db.cash,
        };
        assert!(matches!(
            eliminate(&ctb, &[pair]),
            Err(ConsolidationError::UnknownAccount { .. })
        ));
    }
}
