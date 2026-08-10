//! # gl — reference Accounting / General Ledger implementation on TPT ERP.
//!
//! A full double-entry accounting reference built on the framework's event-sourced
//! ledger, with no global row locks and 100% replayable read models:
//!
//! - [`coa`] — the chart of accounts (account types + normal balances).
//! - [`journal`] — a multi-currency, event-sourced journal engine. Writes append legs
//!   sharded by account, so concurrent postings to different accounts never block on a
//!   global lock; optimistic concurrency is per-account via the event store's sequence.
//!   The running balance is a CQRS read model cached per tenant.
//! - [`fx`] — explicit typed cross-currency conversion (`Money<From> -> Money<To>`),
//!   a point-in-time rate table, and period-end account revaluation.
//! - [`close`] — a `StateMachine`-derived period-close workflow
//!   (`Open -> SoftClose -> Reconciling -> Closed -> Locked`, with a reopen branch), a
//!   trial-balance gate, generated closing/reversing entries, and a `gl.period_closed`
//!   background job.
//! - [`reporting`] — CQRS-replayed Trial Balance, Income Statement, and Balance Sheet
//!   read models cached via `tpt-erp-cache`.

pub mod close;
pub mod coa;
pub mod fx;
pub mod journal;
pub mod reporting;

use tpt_erp_tenant::{TenantId, TenantSlug};

/// Build a tenant for example/demo use.
pub fn demo_tenant() -> TenantId {
    TenantSlug("gl-demo".to_string()).to_id()
}
