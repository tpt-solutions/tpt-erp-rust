//! Chart of accounts.
//!
//! Every posted amount belongs to an [`Account`] of a known [`AccountType`]. The account
//! type determines the account's *normal balance* (which side increases it) and whether
//! it is a permanent (balance-sheet) or temporary (income-statement) account. The
//! reporting module reads these types to build the financial statements.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::marker::PhantomData;
use tpt_erp_ledger::AccountId;
use tpt_erp_primitives::Currency;

/// The five standard GL account categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountType {
    /// Things owned (cash, receivables, inventory). Increases with a debit.
    Asset,
    /// Obligations to others. Increases with a credit.
    Liability,
    /// Owners' residual claim. Increases with a credit.
    Equity,
    /// Income earned. Increases with a credit.
    Revenue,
    /// Costs incurred. Increases with a debit.
    Expense,
}

impl AccountType {
    /// The side that *increases* this account's balance.
    pub fn normal_side(self) -> tpt_erp_ledger::EntrySide {
        match self {
            AccountType::Asset | AccountType::Expense => tpt_erp_ledger::EntrySide::Debit,
            AccountType::Liability | AccountType::Equity | AccountType::Revenue => {
                tpt_erp_ledger::EntrySide::Credit
            }
        }
    }

    /// True for balance-sheet (real) accounts that carry forward; false for the
    /// temporary income-statement accounts closed at period end.
    pub fn is_permanent(self) -> bool {
        !matches!(self, AccountType::Revenue | AccountType::Expense)
    }
}

/// A single GL account in currency `C`.
///
/// The currency is carried as a `PhantomData` marker so an `Account<Usd>` and an
/// `Account<Eur>` are distinct types and cannot be mixed by the compiler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account<C: Currency> {
    pub id: AccountId,
    pub code: String,
    pub name: String,
    pub kind: AccountType,
    #[serde(skip)]
    pub currency: PhantomData<C>,
}

impl<C: Currency> Account<C> {
    /// Create a new account with a generated id.
    pub fn new(code: impl Into<String>, name: impl Into<String>, kind: AccountType) -> Self {
        Self {
            id: AccountId::new(),
            code: code.into(),
            name: name.into(),
            kind,
            currency: PhantomData,
        }
    }

    /// The ISO currency code this account is denominated in.
    pub fn currency_code(&self) -> &'static str {
        C::CODE
    }
}

/// The chart of accounts: the catalog of every account in currency `C`.
#[derive(Debug, Clone)]
pub struct ChartOfAccounts<C: Currency> {
    accounts: HashMap<AccountId, Account<C>>,
}

impl<C: Currency> Default for ChartOfAccounts<C> {
    fn default() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }
}

impl<C: Currency> ChartOfAccounts<C> {
    /// An empty chart of accounts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an account, returning its strong id.
    pub fn add(&mut self, account: Account<C>) -> AccountId {
        let id = account.id;
        self.accounts.insert(id, account);
        id
    }

    /// Look up an account by id.
    pub fn get(&self, id: &AccountId) -> Option<&Account<C>> {
        self.accounts.get(id)
    }

    /// The type of an account, if known.
    pub fn account_type(&self, id: &AccountId) -> Option<AccountType> {
        self.accounts.get(id).map(|a| a.kind)
    }

    /// Iterate the accounts.
    pub fn iter(&self) -> impl Iterator<Item = &Account<C>> {
        self.accounts.values()
    }

    /// Number of accounts.
    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    /// Whether the chart is empty.
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }
}

/// Well-known demo accounts returned by [`demo_coa`].
#[derive(Debug, Clone)]
pub struct DemoAccounts<C: Currency> {
    pub cash: AccountId,
    pub accounts_receivable: AccountId,
    pub inventory: AccountId,
    pub accounts_payable: AccountId,
    pub common_stock: AccountId,
    pub retained_earnings: AccountId,
    pub sales_revenue: AccountId,
    pub cogs: AccountId,
    pub fx_gain_loss: AccountId,
    _c: PhantomData<C>,
}

/// Build a sample chart of accounts in currency `C` (used by examples, tests, and the UI).
pub fn demo_coa<C: Currency>() -> (ChartOfAccounts<C>, DemoAccounts<C>) {
    let mut coa = ChartOfAccounts::new();
    let cash = coa.add(Account::new("1000", "Cash", AccountType::Asset));
    let ar = coa.add(Account::new(
        "1200",
        "Accounts Receivable",
        AccountType::Asset,
    ));
    let inv = coa.add(Account::new("1300", "Inventory", AccountType::Asset));
    let ap = coa.add(Account::new(
        "2000",
        "Accounts Payable",
        AccountType::Liability,
    ));
    let cs = coa.add(Account::new("3000", "Common Stock", AccountType::Equity));
    let re = coa.add(Account::new(
        "3100",
        "Retained Earnings",
        AccountType::Equity,
    ));
    let rev = coa.add(Account::new("4000", "Sales Revenue", AccountType::Revenue));
    let cogs = coa.add(Account::new(
        "5000",
        "Cost of Goods Sold",
        AccountType::Expense,
    ));
    let fx = coa.add(Account::new("5900", "FX Gain/Loss", AccountType::Expense));

    let demo = DemoAccounts {
        cash,
        accounts_receivable: ar,
        inventory: inv,
        accounts_payable: ap,
        common_stock: cs,
        retained_earnings: re,
        sales_revenue: rev,
        cogs,
        fx_gain_loss: fx,
        _c: PhantomData,
    };
    (coa, demo)
}
