//! Financial reporting read models.
//!
//! The Trial Balance, Income Statement, and Balance Sheet are CQRS read models: they are
//! built by folding the posted legs (optionally rebuilt from scratch via
//! [`JournalEngine::rebuild_read_models`](crate::journal::JournalEngine::rebuild_read_models))
//! and cached per tenant via `tpt-erp-cache`. Because the log is the source of truth, a
//! report can always be re-derived and can never silently disagree with the ledger.

use serde::Serialize;
use tpt_erp_ledger::AccountId;
use tpt_erp_primitives::Currency;

use crate::coa::AccountType;
use crate::journal::{AccountBalance, JournalEngine, JournalProjection};

/// One row of the trial balance.
#[derive(Debug, Clone)]
pub struct TrialBalanceRow<C: Currency> {
    pub account: AccountId,
    pub code: String,
    pub name: String,
    pub kind: AccountType,
    pub account_balance: crate::journal::AccountBalance<C>,
    pub signed_balance: tpt_erp_primitives::Money<C>,
}

/// A trial balance: every account's debit/credit totals and signed balance.
#[derive(Debug, Clone)]
pub struct TrialBalance<C: Currency> {
    pub rows: Vec<TrialBalanceRow<C>>,
    pub total_debits: tpt_erp_primitives::Money<C>,
    pub total_credits: tpt_erp_primitives::Money<C>,
}

impl<C: Currency> TrialBalance<C> {
    /// True iff total debits equal total credits (the books balance).
    pub fn is_balanced(&self) -> bool {
        self.total_debits == self.total_credits
    }
}

/// One income-statement line.
#[derive(Debug, Clone)]
pub struct IncomeStatementRow<C: Currency> {
    pub account: AccountId,
    pub name: String,
    pub kind: AccountType,
    pub amount: tpt_erp_primitives::Money<C>,
}

/// An income statement (revenues - expenses) for the period.
#[derive(Debug, Clone)]
pub struct IncomeStatement<C: Currency> {
    pub rows: Vec<IncomeStatementRow<C>>,
    pub total_revenue: tpt_erp_primitives::Money<C>,
    pub total_expense: tpt_erp_primitives::Money<C>,
    pub net_income: tpt_erp_primitives::Money<C>,
}

/// One balance-sheet line.
#[derive(Debug, Clone)]
pub struct BalanceSheetRow<C: Currency> {
    pub code: String,
    pub name: String,
    pub kind: AccountType,
    pub balance: tpt_erp_primitives::Money<C>,
}

/// A balance sheet: assets = liabilities + equity (incl. net income).
#[derive(Debug, Clone)]
pub struct BalanceSheet<C: Currency> {
    pub assets: Vec<BalanceSheetRow<C>>,
    pub liabilities: Vec<BalanceSheetRow<C>>,
    pub equity: Vec<BalanceSheetRow<C>>,
    pub total_assets: tpt_erp_primitives::Money<C>,
    pub total_liabilities: tpt_erp_primitives::Money<C>,
    pub total_equity: tpt_erp_primitives::Money<C>,
    pub net_income: tpt_erp_primitives::Money<C>,
}

impl<C: Currency> BalanceSheet<C> {
    /// True iff assets equal liabilities plus equity (including retained net income).
    pub fn is_balanced(&self) -> bool {
        self.total_assets == self.total_liabilities + self.total_equity
    }
}

/// Build the trial balance from a chart of accounts and a balance projection.
pub fn trial_balance<C: Currency>(
    coa: &crate::coa::ChartOfAccounts<C>,
    proj: &JournalProjection<C>,
) -> TrialBalance<C> {
    let mut rows = Vec::new();
    let mut total_debits = tpt_erp_primitives::Money::<C>::zero();
    let mut total_credits = tpt_erp_primitives::Money::<C>::zero();

    for acc in coa.iter() {
        let bal: AccountBalance<C> = proj.balances.get(&acc.id).copied().unwrap_or_default();
        let balance = bal.signed(acc.kind);
        total_debits += bal.debits;
        total_credits += bal.credits;
        rows.push(TrialBalanceRow {
            account: acc.id,
            code: acc.code.clone(),
            name: acc.name.clone(),
            kind: acc.kind,
            account_balance: bal,
            signed_balance: balance,
        });
    }

    TrialBalance {
        rows,
        total_debits,
        total_credits,
    }
}

/// Build the income statement (temporary accounts) from a projection.
pub fn income_statement<C: Currency>(
    coa: &crate::coa::ChartOfAccounts<C>,
    proj: &JournalProjection<C>,
) -> IncomeStatement<C> {
    let mut rows = Vec::new();
    let mut total_revenue = tpt_erp_primitives::Money::<C>::zero();
    let mut total_expense = tpt_erp_primitives::Money::<C>::zero();

    for acc in coa.iter() {
        let kind = acc.kind;
        if kind.is_permanent() {
            continue;
        }
        let bal: AccountBalance<C> = proj.balances.get(&acc.id).copied().unwrap_or_default();
        let amount = bal.signed(kind);
        if amount == tpt_erp_primitives::Money::zero() {
            continue;
        }
        match kind {
            AccountType::Revenue => total_revenue += amount,
            _ => total_expense += amount,
        }
        rows.push(IncomeStatementRow {
            account: acc.id,
            name: acc.name.clone(),
            kind,
            amount,
        });
    }

    let net_income = total_revenue - total_expense;
    IncomeStatement {
        rows,
        total_revenue,
        total_expense,
        net_income,
    }
}

/// Build the balance sheet (permanent accounts) from a projection, folding net income
/// into equity so the statement balances after closing.
pub fn balance_sheet<C: Currency>(
    coa: &crate::coa::ChartOfAccounts<C>,
    proj: &JournalProjection<C>,
    net_income: tpt_erp_primitives::Money<C>,
) -> BalanceSheet<C> {
    let mut assets = Vec::new();
    let mut liabilities = Vec::new();
    let mut equity = Vec::new();
    let mut total_assets = tpt_erp_primitives::Money::<C>::zero();
    let mut total_liabilities = tpt_erp_primitives::Money::<C>::zero();
    let mut total_equity = tpt_erp_primitives::Money::<C>::zero();

    for acc in coa.iter() {
        let kind = acc.kind;
        if !kind.is_permanent() {
            continue;
        }
        let bal: AccountBalance<C> = proj.balances.get(&acc.id).copied().unwrap_or_default();
        let balance = bal.signed(kind);
        let row = BalanceSheetRow {
            code: acc.code.clone(),
            name: acc.name.clone(),
            kind,
            balance,
        };
        match kind {
            AccountType::Asset => {
                total_assets += balance;
                assets.push(row);
            }
            AccountType::Liability => {
                total_liabilities += balance;
                liabilities.push(row);
            }
            AccountType::Equity => {
                total_equity += balance;
                equity.push(row);
            }
            AccountType::Revenue | AccountType::Expense => unreachable!(),
        }
    }

    // Net income retained in equity.
    total_equity += net_income;

    BalanceSheet {
        assets,
        liabilities,
        equity,
        total_assets,
        total_liabilities,
        total_equity,
        net_income,
    }
}

/// Synchronous trial balance read directly from the engine's live per-account
/// balances (no event-log replay). Useful for UIs without an async runtime.
pub fn trial_balance_now<C>(engine: &JournalEngine<C>) -> TrialBalance<C>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    let mut rows = Vec::new();
    let mut total_debits = tpt_erp_primitives::Money::<C>::zero();
    let mut total_credits = tpt_erp_primitives::Money::<C>::zero();
    for acc in engine.chart_of_accounts().iter() {
        let bal: AccountBalance<C> = engine.balance_of(acc.id);
        let balance = bal.signed(acc.kind);
        total_debits += bal.debits;
        total_credits += bal.credits;
        rows.push(TrialBalanceRow {
            account: acc.id,
            code: acc.code.clone(),
            name: acc.name.clone(),
            kind: acc.kind,
            account_balance: bal,
            signed_balance: balance,
        });
    }
    TrialBalance {
        rows,
        total_debits,
        total_credits,
    }
}

/// Synchronous income statement read from live balances.
pub fn income_statement_now<C>(engine: &JournalEngine<C>) -> IncomeStatement<C>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    let mut rows = Vec::new();
    let mut total_revenue = tpt_erp_primitives::Money::<C>::zero();
    let mut total_expense = tpt_erp_primitives::Money::<C>::zero();
    for acc in engine.chart_of_accounts().iter() {
        if acc.kind.is_permanent() {
            continue;
        }
        let bal: AccountBalance<C> = engine.balance_of(acc.id);
        let amount = bal.signed(acc.kind);
        if amount == tpt_erp_primitives::Money::zero() {
            continue;
        }
        match acc.kind {
            AccountType::Revenue => total_revenue += amount,
            _ => total_expense += amount,
        }
        rows.push(IncomeStatementRow {
            account: acc.id,
            name: acc.name.clone(),
            kind: acc.kind,
            amount,
        });
    }
    let net_income = total_revenue - total_expense;
    IncomeStatement {
        rows,
        total_revenue,
        total_expense,
        net_income,
    }
}

/// Synchronous balance sheet read from live balances.
pub fn balance_sheet_now<C>(engine: &JournalEngine<C>) -> BalanceSheet<C>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    let mut balances = std::collections::HashMap::new();
    for acc in engine.chart_of_accounts().iter() {
        balances.insert(acc.id, engine.balance_of(acc.id));
    }
    let proj = crate::journal::JournalProjection { balances };
    let is = income_statement_now(engine);
    balance_sheet(engine.chart_of_accounts(), &proj, is.net_income)
}

/// Rebuild all three reports from the event log (CQRS replay) and through the engine's
/// cache. This is the production read path; the reports can never drift from the ledger.
pub async fn cached_reports<C>(
    engine: &JournalEngine<C>,
) -> Result<(TrialBalance<C>, IncomeStatement<C>, BalanceSheet<C>), crate::journal::JournalError>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    let proj = engine.rebuild_read_models().await?;
    let coa = engine.chart_of_accounts();
    let tb = trial_balance(coa, &proj);
    let is = income_statement(coa, &proj);
    let bs = balance_sheet(coa, &proj, is.net_income);
    Ok((tb, is, bs))
}

/// Rebuild all three reports for a single accounting `period` (CQRS replay filtered to that
/// period's legs). This is what makes period-scoped reporting correct once a period is closed:
/// only the postings that belong to `period` are folded into the read model.
pub async fn cached_reports_for_period<C>(
    engine: &JournalEngine<C>,
    period: &str,
) -> Result<(TrialBalance<C>, IncomeStatement<C>, BalanceSheet<C>), crate::journal::JournalError>
where
    C: Currency + Serialize + serde::de::DeserializeOwned,
{
    let proj = engine.rebuild_read_models_for_period(period).await?;
    let coa = engine.chart_of_accounts();
    let tb = trial_balance(coa, &proj);
    let is = income_statement(coa, &proj);
    let bs = balance_sheet(coa, &proj, is.net_income);
    Ok((tb, is, bs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use tpt_erp_ledger::{EntrySide, LedgerEntry};
    use tpt_erp_primitives::Usd;

    #[tokio::test]
    async fn reports_balance_and_derive_net_income() {
        let (eng, d) = crate::journal::demo(crate::demo_tenant());

        // A sale: cash 500, revenue 500.
        eng.post_transaction(
            vec![
                LedgerEntry {
                    account: d.cash,
                    side: EntrySide::Debit,
                    amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(500)),
                },
                LedgerEntry {
                    account: d.sales_revenue,
                    side: EntrySide::Credit,
                    amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(500)),
                },
            ],
            "2026-01",
            "sale",
        )
        .await
        .unwrap();

        // An expense: cogs 200, cash 200.
        eng.post_transaction(
            vec![
                LedgerEntry {
                    account: d.cogs,
                    side: EntrySide::Debit,
                    amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(200)),
                },
                LedgerEntry {
                    account: d.cash,
                    side: EntrySide::Credit,
                    amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(200)),
                },
            ],
            "2026-01",
            "expense",
        )
        .await
        .unwrap();

        let (tb, is, bs) = cached_reports(&eng).await.unwrap();
        assert!(tb.is_balanced());
        assert_eq!(is.total_revenue.amount(), Decimal::from(500));
        assert_eq!(is.total_expense.amount(), Decimal::from(200));
        assert_eq!(is.net_income.amount(), Decimal::from(300));

        // Before closing, net income is folded into equity so the sheet balances.
        assert!(bs.is_balanced());
        assert_eq!(bs.total_assets.amount(), Decimal::from(300));
        assert_eq!(bs.total_equity.amount(), Decimal::from(300));
    }
}
