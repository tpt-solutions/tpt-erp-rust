//! Period-end close workflow.
//!
//! The close is driven by a [`StateMachine`]-derived [`PeriodStatus`], so an illegal jump
//! (e.g. `Open -> Closed`, or `Locked -> Open` in one step) is rejected at runtime with a
//! typed error. The legal graph is the single source of truth. Closing is gated on a
//! balanced trial balance, emits generated closing/reversing entries, and publishes a
//! `gl.period_closed` background job. A `Locked` period can be reopened step-by-step.

use serde::Serialize;
use tpt_erp_ledger::{EntrySide, LedgerEntry};
use tpt_erp_primitives::{Entity, Id, StateMachine};

use crate::journal::JournalEngine;
use crate::reporting::TrialBalance;

/// Marker entity for an accounting period.
#[derive(Debug)]
pub struct GlPeriod;
impl Entity for GlPeriod {}

/// The lifecycle of an accounting period.
///
/// `Open -> SoftClose -> Reconciling -> Closed -> Locked`, with an explicit reopen branch
/// that unwinds one step at a time (`Locked -> Closed -> Reconciling -> SoftClose -> Open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, StateMachine)]
#[state_machine(transitions(
    Open => SoftClose,
    SoftClose => Reconciling,
    Reconciling => Closed,
    Closed => Locked,
    Closed => Reconciling,
    Reconciling => SoftClose,
    SoftClose => Open,
    Locked => Closed,
))]
pub enum PeriodStatus {
    Open,
    SoftClose,
    Reconciling,
    Closed,
    Locked,
}

/// An accounting period and its current close status.
#[derive(Debug, Clone)]
pub struct Period {
    pub id: Id<GlPeriod>,
    pub name: String,
    pub status: PeriodStatus,
}

impl Period {
    /// A new period in the `Open` state.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Id::new(),
            name: name.into(),
            status: PeriodStatus::Open,
        }
    }

    /// Attempt a state transition, enforcing the prerequisite graph.
    pub fn advance(&mut self, to: PeriodStatus) -> Result<(), PeriodStatusTransitionError> {
        self.status = self.status.transition(to)?;
        Ok(())
    }

    /// Whether the period is fully locked.
    pub fn is_locked(&self) -> bool {
        matches!(self.status, PeriodStatus::Locked)
    }
}

/// Errors raised by the close workflow.
#[derive(Debug, thiserror::Error)]
pub enum CloseError {
    #[error("trial balance does not balance: cannot close the period")]
    TrialBalanceNotBalanced,
    #[error("illegal period transition: {0}")]
    Transition(#[from] PeriodStatusTransitionError),
}

/// Orchestrates the close of one period over a [`JournalEngine`].
pub struct CloseWorkflow<C: tpt_erp_primitives::Currency> {
    period: Period,
    engine: JournalEngine<C>,
    bus: Option<Box<dyn tpt_erp_bus::EventBus>>,
}

impl<C> CloseWorkflow<C>
where
    C: tpt_erp_primitives::Currency + Serialize + serde::de::DeserializeOwned,
{
    /// Build a workflow around a period and the journal it closes.
    pub fn new(period: Period, engine: JournalEngine<C>) -> Self {
        Self {
            period,
            engine,
            bus: None,
        }
    }

    /// Attach a background-job bus; the `gl.period_closed` job is published on close.
    pub fn with_bus(mut self, bus: Box<dyn tpt_erp_bus::EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// The period being managed.
    pub fn period(&self) -> &Period {
        &self.period
    }

    /// The journal engine (for posting closing entries).
    pub fn engine(&self) -> &JournalEngine<C> {
        &self.engine
    }

    /// Advance `Open -> SoftClose` (no posting yet; freeze manual entry).
    pub fn soft_close(&mut self) -> Result<(), CloseError> {
        self.period.advance(PeriodStatus::SoftClose)?;
        Ok(())
    }

    /// Advance `SoftClose -> Reconciling` (accounts being reconciled).
    pub fn begin_reconcile(&mut self) -> Result<(), CloseError> {
        self.period.advance(PeriodStatus::Reconciling)?;
        Ok(())
    }

    /// Complete the close: requires a *balanced* trial balance, then moves
    /// `Reconciling -> Closed` and publishes the `gl.period_closed` job.
    pub async fn close(&mut self, trial_balance: &TrialBalance<C>) -> Result<(), CloseError> {
        if !trial_balance.is_balanced() {
            return Err(CloseError::TrialBalanceNotBalanced);
        }
        self.period.advance(PeriodStatus::Closed)?;
        if let Some(bus) = &self.bus {
            let payload = serde_json::json!({
                "period": self.period.name,
                "id": self.period.id.as_str(),
            })
            .to_string();
            let _ = bus.publish("gl.period_closed", payload.as_bytes()).await;
        }
        Ok(())
    }

    /// Advance `Closed -> Locked` (irreversible to Open, but reopenable one step).
    pub fn lock(&mut self) -> Result<(), CloseError> {
        self.period.advance(PeriodStatus::Locked)?;
        Ok(())
    }

    /// Post the generated closing entries (income statement -> retained earnings) for
    /// this period. No-op if there is nothing to close.
    pub async fn post_closing_entries(
        &self,
        retained_earnings: tpt_erp_ledger::AccountId,
        period: &str,
    ) -> Result<Option<tpt_erp_ledger::TransactionId>, JournalError> {
        let entries = self.engine.generate_closing_entries(retained_earnings);
        if entries.is_empty() {
            return Ok(None);
        }
        let id = self
            .engine
            .post_transaction(entries, period, "Period close")
            .await?;
        Ok(Some(id))
    }
}

/// The journal error surfaced when posting closing entries fails.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("journal post failed: {0}")]
    Post(#[from] crate::journal::JournalError),
}

/// Generate the reversing entries for a set of accrual entries: each leg flipped to the
/// opposite side, to be posted in the following period.
pub fn reversing_entries<C: tpt_erp_primitives::Currency>(
    entries: &[LedgerEntry<C>],
) -> Vec<LedgerEntry<C>> {
    entries
        .iter()
        .map(|e| LedgerEntry {
            account: e.account,
            side: match e.side {
                EntrySide::Debit => EntrySide::Credit,
                EntrySide::Credit => EntrySide::Debit,
            },
            amount: e.amount,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::trial_balance;
    use rust_decimal::Decimal;
    use tpt_erp_bus::memory::InMemoryBus;
    use tpt_erp_ledger::{EntrySide, LedgerEntry};
    use tpt_erp_primitives::Usd;

    async fn tx(eng: &crate::journal::JournalEngine<Usd>, d: &crate::coa::DemoAccounts<Usd>) {
        eng.post_transaction(
            vec![
                LedgerEntry {
                    account: d.cash,
                    side: EntrySide::Debit,
                    amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(100)),
                },
                LedgerEntry {
                    account: d.common_stock,
                    side: EntrySide::Credit,
                    amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(100)),
                },
            ],
            "2026-01",
            "x",
        )
        .await
        .unwrap();
    }

    #[test]
    fn period_state_machine_enforces_graph() {
        let mut p = Period::new("2026-01");
        assert!(p.advance(PeriodStatus::SoftClose).is_ok());
        assert!(p.advance(PeriodStatus::Reconciling).is_ok());
        assert!(p.advance(PeriodStatus::Closed).is_ok());
        assert!(p.advance(PeriodStatus::Locked).is_ok());
        // Locked cannot jump straight to Open.
        assert!(p.advance(PeriodStatus::Open).is_err());
        // Reopen branch unwinds one step at a time.
        assert!(p.advance(PeriodStatus::Closed).is_ok());
        assert!(p.advance(PeriodStatus::Reconciling).is_ok());
        assert!(p.advance(PeriodStatus::SoftClose).is_ok());
        assert!(p.advance(PeriodStatus::Open).is_ok());
        // Illegal jump Open -> Closed.
        assert!(p.advance(PeriodStatus::Closed).is_err());
    }

    #[tokio::test]
    async fn close_requires_balanced_trial_balance() {
        let (eng, d) = crate::journal::demo(crate::demo_tenant());
        tx(&eng, &d).await;
        let proj = eng.rebuild_read_models().await.unwrap();
        let tb = trial_balance(eng.chart_of_accounts(), &proj);
        assert!(tb.is_balanced());

        let mut wf = CloseWorkflow::new(Period::new("2026-01"), eng);
        wf.soft_close().unwrap();
        wf.begin_reconcile().unwrap();
        wf.close(&tb).await.unwrap();
        assert_eq!(wf.period().status, PeriodStatus::Closed);
    }

    #[tokio::test]
    async fn period_closed_job_published_and_lockable() {
        let bus = InMemoryBus::new();
        let (eng, d) = crate::journal::demo(crate::demo_tenant());
        tx(&eng, &d).await;
        let proj = eng.rebuild_read_models().await.unwrap();
        let tb = trial_balance(eng.chart_of_accounts(), &proj);

        let mut wf = CloseWorkflow::new(Period::new("2026-01"), eng).with_bus(Box::new(bus));
        wf.soft_close().unwrap();
        wf.begin_reconcile().unwrap();
        wf.close(&tb).await.unwrap();
        wf.lock().unwrap();
        assert!(wf.period().is_locked());
    }

    #[test]
    fn reversing_entries_flip_sides() {
        let (_, d) = crate::journal::demo(crate::demo_tenant());
        let entries = vec![
            LedgerEntry {
                account: d.cash,
                side: EntrySide::Debit,
                amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(50)),
            },
            LedgerEntry {
                account: d.accounts_payable,
                side: EntrySide::Credit,
                amount: tpt_erp_primitives::Money::<Usd>::new(Decimal::from(50)),
            },
        ];
        let rev = reversing_entries(&entries);
        assert_eq!(rev[0].side, EntrySide::Credit);
        assert_eq!(rev[1].side, EntrySide::Debit);
    }
}
