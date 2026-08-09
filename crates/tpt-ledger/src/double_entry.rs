//! Double-entry accounting core.
//!
//! Every [`Transaction`] must *balance*: total debits equal total credits in the same
//! currency. The [`DoubleEntry`] trait enforces this at the type level before anything
//! reaches the database. Cross-currency transactions are unrepresentable because a
//! `Transaction` is parameterized by a single [`Currency`].

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tpt_primitives::{Currency, Entity, Id, Money};

/// Marker for a ledger account entity.
#[derive(Debug)]
pub struct Account;
impl Entity for Account {}

/// Strong identifier for a ledger account.
pub type AccountId = Id<Account>;

/// Marker for a posted ledger transaction entity.
#[derive(Debug)]
pub struct LedgerTransaction;
impl Entity for LedgerTransaction {}

/// Strong identifier for a posted transaction.
pub type TransactionId = Id<LedgerTransaction>;

/// Which side of the ledger an entry sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntrySide {
    Debit,
    Credit,
}

/// A single leg of a transaction: an account, a side, and an amount.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LedgerEntry<C: Currency> {
    pub account: AccountId,
    pub side: EntrySide,
    pub amount: Money<C>,
}

/// A balanced (or candidate) double-entry transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction<C: Currency> {
    pub id: TransactionId,
    pub entries: Vec<LedgerEntry<C>>,
}

/// Errors from double-entry validation.
#[derive(Debug, Error)]
pub enum DoubleEntryError {
    #[error("transaction {tx} is unbalanced: debits {debits} != credits {credits}")]
    Unbalanced {
        tx: TransactionId,
        debits: String,
        credits: String,
    },
    #[error("transaction {tx} must contain at least two entries")]
    Empty { tx: TransactionId },
}

/// Behaviour shared by anything that must obey double-entry rules.
pub trait DoubleEntry {
    type Currency: Currency;

    fn entries(&self) -> &[LedgerEntry<Self::Currency>];

    fn debits(&self) -> Money<Self::Currency> {
        self.entries()
            .iter()
            .filter(|e| e.side == EntrySide::Debit)
            .map(|e| e.amount)
            .fold(Money::zero(), |acc, amt| acc + amt)
    }

    fn credits(&self) -> Money<Self::Currency> {
        self.entries()
            .iter()
            .filter(|e| e.side == EntrySide::Credit)
            .map(|e| e.amount)
            .fold(Money::zero(), |acc, amt| acc + amt)
    }

    /// True iff debits equal credits.
    fn is_balanced(&self) -> bool {
        self.debits() == self.credits()
    }

    /// Validate balance (and non-emptiness), returning a descriptive error otherwise.
    fn validate(&self) -> Result<(), DoubleEntryError> {
        let id = self.id();
        if self.entries().len() < 2 {
            return Err(DoubleEntryError::Empty { tx: id });
        }
        if self.is_balanced() {
            Ok(())
        } else {
            Err(DoubleEntryError::Unbalanced {
                tx: id,
                debits: self.debits().to_string(),
                credits: self.credits().to_string(),
            })
        }
    }

    /// The transaction id, if available (used for error reporting).
    fn id(&self) -> TransactionId;
}

impl<C: Currency> DoubleEntry for Transaction<C> {
    type Currency = C;

    fn entries(&self) -> &[LedgerEntry<C>] {
        &self.entries
    }

    fn id(&self) -> TransactionId {
        self.id
    }
}

/// A domain event carrying posted transactions through the event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LedgerEvent<C: Currency> {
    TransactionPosted(Transaction<C>),
}

impl<C: Currency> LedgerEvent<C> {
    /// Reconstruct a ledger event from a stored event's JSON payload.
    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, serde_json::Error>
    where
        C: serde::de::DeserializeOwned,
    {
        serde_json::from_value(payload.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use tpt_primitives::Usd;

    fn tx(debit: Decimal, credit: Decimal) -> Transaction<Usd> {
        let a = AccountId::new();
        let b = AccountId::new();
        Transaction {
            id: TransactionId::new(),
            entries: vec![
                LedgerEntry {
                    account: a,
                    side: EntrySide::Debit,
                    amount: Money::new(debit),
                },
                LedgerEntry {
                    account: b,
                    side: EntrySide::Credit,
                    amount: Money::new(credit),
                },
            ],
        }
    }

    #[test]
    fn balanced_transaction_validates() {
        let t = tx(Decimal::from(100), Decimal::from(100));
        assert!(t.is_balanced());
        assert!(t.validate().is_ok());
    }

    #[test]
    fn unbalanced_transaction_rejected() {
        let t = tx(Decimal::from(100), Decimal::from(99));
        assert!(!t.is_balanced());
        let err = t.validate().unwrap_err();
        assert!(matches!(err, DoubleEntryError::Unbalanced { .. }));
    }

    #[test]
    fn single_entry_rejected_as_empty() {
        let t = Transaction {
            id: TransactionId::new(),
            entries: vec![LedgerEntry {
                account: AccountId::new(),
                side: EntrySide::Debit,
                amount: Money::<Usd>::new(Decimal::from(5)),
            }],
        };
        assert!(matches!(t.validate(), Err(DoubleEntryError::Empty { .. })));
    }

    #[test]
    fn event_roundtrips_through_json() {
        let ev = LedgerEvent::TransactionPosted(tx(Decimal::from(50), Decimal::from(50)));
        let json = serde_json::to_value(&ev).unwrap();
        let back = LedgerEvent::<Usd>::from_payload(&json).unwrap();
        let balanced = match back {
            LedgerEvent::TransactionPosted(inner) => inner.validate().is_ok(),
        };
        assert!(balanced);
    }
}
