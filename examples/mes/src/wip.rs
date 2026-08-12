//! Shop-floor Work-In-Process (WIP) state machine.
//!
//! WIP states are modeled with the [`StateMachine`] derive from `tpt-erp-primitives`,
//! so illegal transitions (e.g. jumping `Raw` straight to `Assembled`, or `Finished`
//! back to `Raw`) are rejected at runtime with a typed error — and the valid graph is
//! the single source of truth.
//!
//! Inspection failures may be reworked: from `Inspected` a unit can go to `Reworking`
//! and re-enter production at `Assembled`, then be re-inspected. The number of rework
//! cycles is bounded ([`MAX_REWORK_CYCLES`]) so a unit that cannot be brought back into
//! spec is forced to `Scrapped` or `Quarantined` instead of looping forever.

use crate::defects::DefectFinding;
use thiserror::Error;
use tpt_erp_primitives::{Entity, Id, StateMachine};

/// A shop-floor item being manufactured.
#[derive(Debug)]
pub struct Wip;
impl Entity for Wip {}

/// Maximum number of rework cycles permitted before a unit must be scrapped or
/// quarantined.
pub const MAX_REWORK_CYCLES: u32 = 3;

/// The lifecycle of a work-in-process item on the shop floor.
///
/// `Raw` material may go to `Machined` *or* `Welded`; both must converge on `Assembled`
/// before `Inspected`. Inspection yields `Finished`, `Scrapped`, or — on a recorded
/// defect — `Reworking`. A reworked unit re-enters production at `Assembled` and is
/// re-inspected; once the rework budget is exhausted it must be `Scrapped` or
/// `Quarantined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, StateMachine)]
#[state_machine(transitions(
    Raw => Machined,
    Raw => Welded,
    Machined => Assembled,
    Welded => Assembled,
    Assembled => Inspected,
    Inspected => Finished,
    Inspected => Scrapped,
    Inspected => Quarantined,
    Inspected => Reworking,
    Reworking => Assembled,
    Reworking => Quarantined,
))]
pub enum WipState {
    Raw,
    Machined,
    Welded,
    Assembled,
    Inspected,
    Reworking,
    Quarantined,
    Finished,
    Scrapped,
}

/// Error returned when a rework routing cannot be performed.
#[derive(Debug, Error)]
pub enum ReworkError {
    /// The rework budget is exhausted; the unit must be scrapped or quarantined.
    #[error("rework limit of {max} cycles exceeded; the unit must be scrapped or quarantined")]
    LimitExceeded { max: u32 },
    /// The requested transition is illegal for the current state.
    #[error(transparent)]
    IllegalTransition(#[from] WipStateTransitionError),
}

/// A tracked WIP item.
#[derive(Debug, Clone)]
pub struct WipItem {
    pub id: Id<Wip>,
    pub state: WipState,
    /// Number of times this unit has entered the `Reworking` state.
    pub rework_count: u32,
    /// Defect findings recorded against this unit across all inspections.
    pub defects: Vec<DefectFinding>,
}

impl WipItem {
    /// Create a new item in the `Raw` state.
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            state: WipState::Raw,
            rework_count: 0,
            defects: Vec::new(),
        }
    }

    /// Attempt a state transition, enforcing the prerequisite graph.
    pub fn advance(&mut self, to: WipState) -> Result<(), WipStateTransitionError> {
        self.state = self.state.transition(to)?;
        Ok(())
    }

    /// Record a defect finding against this unit.
    pub fn record_defect(&mut self, finding: DefectFinding) {
        self.defects.push(finding);
    }

    /// Route a failed inspection into rework, consuming one rework cycle.
    ///
    /// Fails with [`ReworkError::LimitExceeded`] once [`MAX_REWORK_CYCLES`] have been
    /// used; the caller must then scrap or quarantine the unit.
    pub fn enter_rework(&mut self) -> Result<(), ReworkError> {
        if self.rework_count >= MAX_REWORK_CYCLES {
            return Err(ReworkError::LimitExceeded {
                max: MAX_REWORK_CYCLES,
            });
        }
        self.state = self.state.transition(WipState::Reworking)?;
        self.rework_count += 1;
        Ok(())
    }

    /// Complete a rework cycle, re-entering production at `Assembled`.
    pub fn complete_rework(&mut self) -> Result<(), WipStateTransitionError> {
        self.state = self.state.transition(WipState::Assembled)?;
        Ok(())
    }

    /// Accept the unit (inspection passed or use-as-is disposition).
    pub fn finish(&mut self) -> Result<(), WipStateTransitionError> {
        self.state = self.state.transition(WipState::Finished)?;
        Ok(())
    }

    /// Scrap the unit.
    pub fn scrap(&mut self) -> Result<(), WipStateTransitionError> {
        self.state = self.state.transition(WipState::Scrapped)?;
        Ok(())
    }

    /// Quarantine the unit for engineering disposition.
    pub fn quarantine(&mut self) -> Result<(), WipStateTransitionError> {
        self.state = self.state.transition(WipState::Quarantined)?;
        Ok(())
    }

    /// Whether the item is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            WipState::Finished | WipState::Scrapped | WipState::Quarantined
        )
    }
}

impl Default for WipItem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_can_go_to_machined_or_welded() {
        assert!(WipState::Raw.can_transition(WipState::Machined));
        assert!(WipState::Raw.can_transition(WipState::Welded));
        assert!(!WipState::Raw.can_transition(WipState::Assembled));
    }

    #[test]
    fn prerequisite_verification_blocks_illegal_jumps() {
        let mut item = WipItem::new();
        // Cannot assemble before machining/welding.
        let err = item.advance(WipState::Assembled).unwrap_err();
        assert_eq!(err.from, WipState::Raw);
        assert_eq!(err.to, WipState::Assembled);

        // Legal path: Raw -> Machined -> Assembled -> Inspected -> Finished.
        item.advance(WipState::Machined).unwrap();
        item.advance(WipState::Assembled).unwrap();
        item.advance(WipState::Inspected).unwrap();
        item.advance(WipState::Finished).unwrap();
        assert!(item.is_terminal());

        // No transitions out of a terminal state.
        assert!(!item.state.can_transition(WipState::Raw));
    }

    #[test]
    fn welded_path_converges_to_assembled() {
        let mut item = WipItem::new();
        item.advance(WipState::Welded).unwrap();
        item.advance(WipState::Assembled).unwrap();
        assert!(item.state.can_transition(WipState::Inspected));
    }

    #[test]
    fn scrap_is_a_terminal_branch() {
        let mut item = WipItem::new();
        item.advance(WipState::Machined).unwrap();
        item.advance(WipState::Assembled).unwrap();
        item.advance(WipState::Inspected).unwrap();
        item.advance(WipState::Scrapped).unwrap();
        assert!(item.is_terminal());
    }

    #[test]
    fn state_machine_rejects_illegal_rework_transitions() {
        // A raw item cannot jump straight into rework.
        let mut item = WipItem::new();
        assert!(!WipState::Raw.can_transition(WipState::Reworking));
        assert!(item.advance(WipState::Reworking).is_err());

        // Reworking must re-enter production at Assembled, never skip to Finished.
        let mut item = WipItem::new();
        item.advance(WipState::Machined).unwrap();
        item.advance(WipState::Assembled).unwrap();
        item.advance(WipState::Inspected).unwrap();
        item.enter_rework().unwrap();
        assert!(!WipState::Reworking.can_transition(WipState::Finished));
        assert!(item.advance(WipState::Finished).is_err());
        // Legal rework re-entry.
        assert!(WipState::Reworking.can_transition(WipState::Assembled));
    }

    #[test]
    fn defect_recording_attaches_to_unit() {
        let mut item = WipItem::new();
        let finding = crate::defects::lookup_code("SUR-002")
            .unwrap()
            .finding(None, "pitting observed");
        item.record_defect(finding.clone());
        assert_eq!(item.defects.len(), 1);
        assert_eq!(item.defects[0].id, finding.id);
        assert_eq!(item.defects[0].code.code, "SUR-002");
    }

    #[test]
    fn exceeded_rework_limit_forces_scrap_or_quarantine() {
        let mut item = WipItem::new();
        item.advance(WipState::Machined).unwrap();
        item.advance(WipState::Assembled).unwrap();

        for cycle in 0..MAX_REWORK_CYCLES {
            item.advance(WipState::Inspected).unwrap();
            // Each cycle consumes one rework entry.
            item.enter_rework().unwrap();
            assert_eq!(item.rework_count, cycle + 1);
            // Fix it and re-enter production.
            item.complete_rework().unwrap();
        }

        // Back at Assembled; one more inspection then a rework attempt must be refused.
        item.advance(WipState::Inspected).unwrap();
        assert_eq!(item.rework_count, MAX_REWORK_CYCLES);
        let err = item.enter_rework().unwrap_err();
        assert!(matches!(err, ReworkError::LimitExceeded { .. }));
        // The unit is still in Inspected; it must be scrapped or quarantined.
        assert!(!item.is_terminal());
        item.scrap().unwrap();
        assert!(item.is_terminal());
    }

    #[test]
    fn disposition_drives_terminal_state() {
        use crate::defects::{Disposition, lookup_code, recommended_disposition};

        // Critical defect -> scrap.
        let mut item = WipItem::new();
        item.advance(WipState::Machined).unwrap();
        item.advance(WipState::Assembled).unwrap();
        item.advance(WipState::Inspected).unwrap();
        let crit = lookup_code("ELE-001").unwrap().finding(None, "open");
        assert_eq!(recommended_disposition(std::slice::from_ref(&crit)), Disposition::Scrap);
        item.record_defect(crit);
        item.scrap().unwrap();
        assert!(item.is_terminal());
    }
}
