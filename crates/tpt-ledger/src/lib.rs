//! # tpt-ledger (scaffold)
//!
//! The financial and audit heart of TPT ERP. Planned for Phase 2:
//!
//! - **Event store**: append-only, immutable event log with optimistic concurrency.
//! - **Double-entry core**: a trait enforcing that every transaction balances, checked
//!   *before* it hits the database.
//! - **CQRS projection engine**: asynchronous projectors that build read-models and
//!   support replay-from-scratch.
//!
//! This crate currently exposes shared error types; the engine lands in Phase 2.

use thiserror::Error;

/// Errors for ledger operations.
#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("transaction does not balance: debits {debits} != credits {credits}")]
    Unbalanced { debits: String, credits: String },
    #[error("optimistic concurrency conflict at sequence {expected}, found {actual}")]
    Conflict { expected: u64, actual: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let e = LedgerError::Unbalanced {
            debits: "10".into(),
            credits: "9".into(),
        };
        assert!(e.to_string().contains("does not balance"));
    }
}
