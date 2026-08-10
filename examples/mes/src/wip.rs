//! Shop-floor Work-In-Process (WIP) state machine.
//!
//! WIP states are modeled with the [`StateMachine`] derive from `tpt-erp-primitives`, so
//! illegal transitions (e.g. jumping `Raw` straight to `Assembled`, or `Finished` back to
//! `Raw`) are rejected at runtime with a typed error — and the valid graph is the single
//! source of truth.

use tpt_erp_primitives::{Entity, Id, StateMachine};

/// A shop-floor item being manufactured.
#[derive(Debug)]
pub struct Wip;
impl Entity for Wip {}

/// The lifecycle of a work-in-process item on the shop floor.
///
/// `Raw` material may go to `Machined` *or* `Welded`; both must converge on `Assembled`
/// before `Inspected`, and inspection yields either `Finished` or `Scrapped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, StateMachine)]
#[state_machine(transitions(
    Raw => Machined,
    Raw => Welded,
    Machined => Assembled,
    Welded => Assembled,
    Assembled => Inspected,
    Inspected => Finished,
    Inspected => Scrapped,
))]
pub enum WipState {
    Raw,
    Machined,
    Welded,
    Assembled,
    Inspected,
    Finished,
    Scrapped,
}

/// A tracked WIP item.
#[derive(Debug, Clone)]
pub struct WipItem {
    pub id: Id<Wip>,
    pub state: WipState,
}

impl WipItem {
    /// Create a new item in the `Raw` state.
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            state: WipState::Raw,
        }
    }

    /// Attempt a state transition, enforcing the prerequisite graph.
    pub fn advance(&mut self, to: WipState) -> Result<(), WipStateTransitionError> {
        self.state = self.state.transition(to)?;
        Ok(())
    }

    /// Whether the item is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, WipState::Finished | WipState::Scrapped)
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
}
