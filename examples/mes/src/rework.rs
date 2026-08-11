//! Rework orchestration for the MES shop floor.
//!
//! This module ties the defect taxonomy ([`crate::defects`]) to the WIP state machine
//! ([`crate::wip`]). [`inspect_unit`] records the inspector's findings against a unit,
//! derives the recommended [`Disposition`] from defect severity, and routes the unit:
//! `Scrap` → `Scrapped`, `UseAsIs` → `Finished`, `Rework` → `Reworking` (bounded by
//! [`crate::wip::MAX_REWORK_CYCLES`], after which the caller must scrap or quarantine).

use crate::defects::{DefectFinding, Disposition, recommended_disposition};
use crate::wip::{ReworkError, WipItem, WipState};

/// Drive a unit through the initial production path to its first inspection
/// (`Raw` → `Machined` → `Assembled` → `Inspected`).
pub fn produce(item: &mut WipItem) -> Result<(), ReworkError> {
    item.advance(WipState::Machined)
        .map_err(ReworkError::IllegalTransition)?;
    item.advance(WipState::Assembled)
        .map_err(ReworkError::IllegalTransition)?;
    item.advance(WipState::Inspected)
        .map_err(ReworkError::IllegalTransition)?;
    Ok(())
}

/// After a rework cycle (`Reworking`), re-enter production and re-inspect
/// (`Reworking` → `Assembled` → `Inspected`).
pub fn reinspect(item: &mut WipItem) -> Result<(), ReworkError> {
    item.complete_rework()
        .map_err(ReworkError::IllegalTransition)?;
    item.advance(WipState::Inspected)
        .map_err(ReworkError::IllegalTransition)?;
    Ok(())
}

/// Inspect a unit that is currently in the `Inspected` state.
///
/// Findings are recorded against the unit, the worst-severity disposition is computed,
/// and the unit is routed accordingly. Returns the disposition that was applied.
///
/// On a `Rework` disposition the rework budget is enforced: once it is exhausted this
/// returns [`ReworkError::LimitExceeded`] and leaves the unit in `Inspected` so the
/// caller can scrap or quarantine it.
pub fn inspect_unit(
    item: &mut WipItem,
    findings: Vec<DefectFinding>,
) -> Result<Disposition, ReworkError> {
    let disposition = recommended_disposition(&findings);
    for f in &findings {
        item.record_defect(f.clone());
    }
    match disposition {
        Disposition::Scrap => item.scrap()?,
        Disposition::UseAsIs => item.finish()?,
        Disposition::Rework => item.enter_rework()?,
    }
    Ok(disposition)
}

/// Run a unit through production, inspection, and any necessary rework cycles until it
/// is accepted (`UseAsIs`/`Scrap`) or the rework budget is exhausted.
///
/// `findings_for` supplies the findings for the *n*-th inspection (0-based). Returning
/// an empty vector simulates a clean re-inspection after a successful fix.
pub fn run_rework_loop<F>(
    item: &mut WipItem,
    mut findings_for: F,
) -> Result<Disposition, ReworkError>
where
    F: FnMut(usize) -> Vec<DefectFinding>,
{
    produce(item)?;
    let mut n = 0;
    loop {
        let disposition = inspect_unit(item, findings_for(n))?;
        n += 1;
        match disposition {
            Disposition::Rework => reinspect(item)?,
            other => return Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defects::lookup_code;
    use crate::wip::MAX_REWORK_CYCLES;

    #[test]
    fn critical_defect_is_scrapped() {
        let mut item = WipItem::new();
        let crit = lookup_code("ELE-001")
            .unwrap()
            .finding(None, "open circuit");
        let d = run_rework_loop(&mut item, |_| vec![crit.clone()]).unwrap();
        assert_eq!(d, Disposition::Scrap);
        assert_eq!(item.state, WipState::Scrapped);
        assert!(item.is_terminal());
        assert_eq!(item.defects.len(), 1);
    }

    #[test]
    fn minor_defect_is_use_as_is() {
        let mut item = WipItem::new();
        let minor = lookup_code("DIM-002").unwrap().finding(None, "barely off");
        let d = run_rework_loop(&mut item, |_| vec![minor.clone()]).unwrap();
        assert_eq!(d, Disposition::UseAsIs);
        assert_eq!(item.state, WipState::Finished);
        assert!(item.is_terminal());
    }

    #[test]
    fn reworkable_defect_loops_until_fixed() {
        let mut item = WipItem::new();
        let reworkable = lookup_code("SUR-002").unwrap().finding(None, "pitting");
        // First inspection fails (reworkable); subsequent re-inspection is clean.
        let d = run_rework_loop(&mut item, |n| {
            if n == 0 {
                vec![reworkable.clone()]
            } else {
                vec![]
            }
        })
        .unwrap();
        assert_eq!(d, Disposition::UseAsIs);
        assert_eq!(item.rework_count, 1);
        assert_eq!(item.state, WipState::Finished);
        assert!(item.is_terminal());
    }

    #[test]
    fn rework_limit_forces_scrap_or_quarantine() {
        let mut item = WipItem::new();
        let reworkable = lookup_code("SUR-002").unwrap().finding(None, "persistent");
        // Every inspection keeps failing with the reworkable defect.
        let err = run_rework_loop(&mut item, |_| vec![reworkable.clone()]).unwrap_err();
        assert!(matches!(err, ReworkError::LimitExceeded { .. }));
        assert_eq!(item.rework_count, MAX_REWORK_CYCLES);
        // The unit must now be scrapped or quarantined.
        item.scrap().unwrap();
        assert!(item.is_terminal());
    }

    #[test]
    fn illegal_rework_transition_rejected_by_state_machine() {
        // A unit still in `Raw` cannot be routed into rework.
        let mut item = WipItem::new();
        let reworkable = lookup_code("SUR-002").unwrap().finding(None, "x");
        let err = inspect_unit(&mut item, vec![reworkable]).unwrap_err();
        assert!(matches!(err, ReworkError::IllegalTransition(_)));
        // No transition occurred.
        assert_eq!(item.state, WipState::Raw);
    }
}
