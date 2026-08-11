//! Driver Hours-of-Service (HOS) state machine and rule enforcement.
//!
//! A driver's duty status is a [`StateMachine`]-derived [`HosStatus`] so an illegal jump
//! (e.g. `Driving -> SleeperBerth` without first going `OnDuty`, or `OffDuty -> Driving`
//! skipping `OnDuty`) is a typed runtime error. The legal graph is the single source of
//! truth:
//!
//! ```text
//! OffDuty ⇄ OnDuty ⇄ Driving
//!   ↑  ↓        ↑  ↓
//!   └── SleeperBerth ──┘
//! ```
//!
//! On top of the state graph, [`HosClock`] enforces the U.S. std rule: at most **11
//! hours** of driving and at most **14 hours** on-duty (driving + on-duty) within a
//! window that resets after a **10-hour** off-duty (or **8-hour** sleeper-berth) break.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tpt_erp_primitives::{Entity, Id, StateMachine};

/// Marker entity for a driver.
#[derive(Debug)]
pub struct Driver;
impl Entity for Driver {}

/// A driver's duty status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, StateMachine)]
#[state_machine(transitions(
    OffDuty => OnDuty,
    OnDuty => OffDuty,
    OnDuty => Driving,
    Driving => OnDuty,
    OnDuty => SleeperBerth,
    SleeperBerth => OnDuty,
    Driving => SleeperBerth,
    SleeperBerth => OffDuty,
))]
pub enum HosStatus {
    /// Not working (resets the on-duty/14-hr window after sufficient duration).
    OffDuty,
    /// Working but not driving (loading, inspections, paperwork).
    OnDuty,
    /// Operating the vehicle (counts toward the 11-hr driving limit).
    Driving,
    /// Resting in the berth (resets the window after sufficient duration).
    SleeperBerth,
}

/// A single contiguous duty period: a status held for a duration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DutyPeriod {
    pub status: HosStatus,
    pub duration: Duration,
}

/// Accumulated duty time within the current (post-reset) window.
#[derive(Debug, Clone, Copy, Default)]
pub struct HosWindow {
    /// Cumulative driving time in the window (must stay ≤ 11h).
    pub driving: Duration,
    /// Cumulative on-duty time (driving + on-duty) in the window (must stay ≤ 14h).
    pub on_duty: Duration,
}

/// Violations of the HOS limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HosViolation {
    #[error("driving limit exceeded: {0:?} > 11h")]
    DrivingLimit(Duration),
    #[error("on-duty limit exceeded: {0:?} > 14h")]
    OnDutyLimit(Duration),
}

/// Per-driver HOS accumulator. Feeds duty periods and reports limit violations.
#[derive(Debug, Clone)]
pub struct HosClock {
    pub driver: Id<Driver>,
    pub status: HosStatus,
    pub window: HosWindow,
}

impl HosClock {
    /// A fresh clock for `driver`, off-duty with an empty window.
    pub fn new(driver: Id<Driver>) -> Self {
        Self {
            driver,
            status: HosStatus::OffDuty,
            window: HosWindow::default(),
        }
    }

    /// Transition the duty status, enforcing the state graph.
    pub fn transition(&mut self, to: HosStatus) -> Result<(), HosStatusTransitionError> {
        self.status = self.status.transition(to)?;
        Ok(())
    }

    /// Apply a duty period, resetting the window when a qualifying rest completes.
    ///
    /// * A `SleeperBerth` period ≥ 8h resets the window.
    /// * An `OffDuty` period ≥ 10h resets the window.
    /// * Otherwise the period's time accrues to the driving/on-duty totals.
    pub fn apply(&mut self, period: DutyPeriod) -> Result<(), HosStatusTransitionError> {
        self.transition(period.status)?;

        let resets = matches!(period.status, HosStatus::OffDuty)
            && period.duration >= Duration::from_secs(10 * 3600)
            || matches!(period.status, HosStatus::SleeperBerth)
                && period.duration >= Duration::from_secs(8 * 3600);

        if resets {
            self.window = HosWindow::default();
            return Ok(());
        }

        match period.status {
            // Driving counts toward both the 11h driving and the 14h on-duty limits.
            HosStatus::Driving => {
                self.window.driving += period.duration;
                self.window.on_duty += period.duration;
            }
            // On-duty (not driving) counts only toward the 14h on-duty limit.
            HosStatus::OnDuty => {
                self.window.on_duty += period.duration;
            }
            // Off-duty / sleeper-berth below the rest threshold does not accrue.
            HosStatus::OffDuty | HosStatus::SleeperBerth => {}
        }
        Ok(())
    }

    /// Whether the current window violates the 11h driving / 14h on-duty limits.
    pub fn violations(&self) -> Vec<HosViolation> {
        let mut v = Vec::new();
        if self.window.driving > Duration::from_secs(11 * 3600) {
            v.push(HosViolation::DrivingLimit(self.window.driving));
        }
        if self.window.on_duty > Duration::from_secs(14 * 3600) {
            v.push(HosViolation::OnDutyLimit(self.window.on_duty));
        }
        v
    }

    /// Whether the current window is within all HOS limits.
    pub fn is_compliant(&self) -> bool {
        self.violations().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(d: u64) -> Duration {
        Duration::from_secs(d * 3600)
    }

    #[test]
    fn state_machine_allows_legal_duty_graph() {
        let driver = Id::new();
        let mut c = HosClock::new(driver);
        c.transition(HosStatus::OnDuty).unwrap();
        c.transition(HosStatus::Driving).unwrap();
        c.transition(HosStatus::OnDuty).unwrap();
        c.transition(HosStatus::SleeperBerth).unwrap();
        c.transition(HosStatus::OffDuty).unwrap();
        // Illegal jump: OffDuty -> Driving skips OnDuty.
        assert!(c.transition(HosStatus::Driving).is_err());
    }

    #[test]
    fn accrues_driving_and_on_duty_within_limits() {
        let mut c = HosClock::new(Id::new());
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: h(1),
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::Driving,
            duration: h(10),
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: h(2),
        })
        .unwrap();
        assert_eq!(c.window.driving, h(10));
        assert_eq!(c.window.on_duty, h(13));
        assert!(c.is_compliant());
    }

    #[test]
    fn driving_over_11_hours_violates() {
        let mut c = HosClock::new(Id::new());
        // A driver must come on-duty before driving (OffDuty -> OnDuty -> Driving).
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: Duration::ZERO,
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::Driving,
            duration: h(12),
        })
        .unwrap();
        assert!(!c.is_compliant());
        assert!(matches!(c.violations()[0], HosViolation::DrivingLimit(_)));
    }

    #[test]
    fn on_duty_over_14_hours_violates() {
        let mut c = HosClock::new(Id::new());
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: h(3),
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::Driving,
            duration: h(11),
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: h(2),
        })
        .unwrap();
        assert!(!c.is_compliant());
        assert!(matches!(c.violations()[0], HosViolation::OnDutyLimit(_)));
    }

    #[test]
    fn ten_hour_off_duty_resets_window() {
        let mut c = HosClock::new(Id::new());
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: Duration::ZERO,
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::Driving,
            duration: h(12),
        })
        .unwrap();
        assert!(!c.is_compliant());
        // A qualifying 10h off-duty break resets the window to zero. Driving must first
        // return to OnDuty, then OffDuty (the legal duty graph has no direct Driving->OffDuty).
        c.apply(DutyPeriod {
            status: HosStatus::OnDuty,
            duration: Duration::ZERO,
        })
        .unwrap();
        c.apply(DutyPeriod {
            status: HosStatus::OffDuty,
            duration: h(10),
        })
        .unwrap();
        assert!(c.is_compliant());
        assert_eq!(c.window.driving, h(0));
    }
}
