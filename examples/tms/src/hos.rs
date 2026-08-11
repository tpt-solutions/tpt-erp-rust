//! Driver Hours-of-Service (HOS) state machine and live rule enforcement.
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
//! Two complementary views sit on top of the state graph:
//!
//! * [`HosClock`] — the **retrospective** accumulator. Feed it [`DutyPeriod`]s (plain
//!   durations) and it reports the 11-hour driving / 14-hour on-duty window limits.
//! * [`HosLog`] — the **live** enforcement engine. Duty status is modelled as a
//!   time-stamped [`DutyInterval`] time-series. From the log it computes the rolling
//!   11/14-hour window, the 30-minute break requirement after 8 hours of cumulative
//!   driving, and the 60/70-hour cycle limit across the rolling 7/8-day window, emitting a
//!   [`HosEscalation`] (and publishing to `tms.hos.escalation` on a bus) whenever a rule is
//!   about to be or is breached. Everything is pure, deterministic and event-sourced: each
//!   interval is an immutable fact and all limits are recomputed from the series.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tpt_erp_bus::EventBus;
use tpt_erp_primitives::{Entity, Id, StateMachine};

/// Marker entity for a driver.
#[derive(Debug)]
pub struct Driver;
impl Entity for Driver {}

/// U.S. FMCSA HOS limits (property-carrying).
const H11: Duration = Duration::from_secs(11 * 3600);
const H14: Duration = Duration::from_secs(14 * 3600);
/// 8 cumulative hours of driving before a 30-minute break is required.
const H8_BREAK: Duration = Duration::from_secs(8 * 3600);
/// A qualifying break is an off-duty/sleeper-berth period of at least 30 minutes.
const H30_BREAK: Duration = Duration::from_secs(30 * 60);
/// A qualifying rest that closes the 11/14-hour window.
const H10_OFF: Duration = Duration::from_secs(10 * 3600);
const H8_SLEEP: Duration = Duration::from_secs(8 * 3600);
/// 60-hour / 70-hour cycle limits in 7 / 8 consecutive days.
const H60: Duration = Duration::from_secs(60 * 3600);
const H70: Duration = Duration::from_secs(70 * 3600);

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

impl HosStatus {
    /// Whether time in this status accrues toward on-duty (and thus the cycle) totals.
    pub fn is_on_duty(self) -> bool {
        !matches!(self, HosStatus::OffDuty | HosStatus::SleeperBerth)
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum HosViolation {
    #[error("driving limit exceeded: {0:?} > 11h")]
    DrivingLimit(Duration),
    #[error("on-duty limit exceeded: {0:?} > 14h")]
    OnDutyLimit(Duration),
    /// 30-minute break required after 8 cumulative hours of driving.
    #[error("30-minute break required: {0:?} driving since last break")]
    BreakRequired(Duration),
    /// 60/70-hour cycle limit exceeded in a 7/8-day rolling window.
    #[error("cycle limit exceeded in {days}-day window: {used:?} > {limit:?}")]
    CycleLimit {
        used: Duration,
        limit: Duration,
        days: u32,
    },
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

        let resets = matches!(period.status, HosStatus::OffDuty) && period.duration >= H10_OFF
            || matches!(period.status, HosStatus::SleeperBerth) && period.duration >= H8_SLEEP;

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
        if self.window.driving > H11 {
            v.push(HosViolation::DrivingLimit(self.window.driving));
        }
        if self.window.on_duty > H14 {
            v.push(HosViolation::OnDutyLimit(self.window.on_duty));
        }
        v
    }

    /// Whether the current window is within all HOS limits.
    pub fn is_compliant(&self) -> bool {
        self.violations().is_empty()
    }
}

/// A sealed duty interval with concrete timestamps — the event-sourced HOS time series.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DutyInterval {
    pub status: HosStatus,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl DutyInterval {
    /// The signed length of the interval (always non-negative when well formed).
    pub fn duration(&self) -> Duration {
        (self.end - self.start).to_std().unwrap_or(Duration::ZERO)
    }
}

/// Escalation categories raised to a dispatcher when a rule is breached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum HosEscalationKind {
    #[error("11-hour driving limit exceeded")]
    DrivingLimitExceeded,
    #[error("14-hour on-duty window exceeded")]
    OnDutyLimitExceeded,
    #[error("30-minute break required")]
    BreakRequired,
    #[error("60/70-hour cycle limit exceeded")]
    CycleLimitExceeded,
}

/// A dispatcher escalation record produced when an HOS rule is (about to be) breached.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HosEscalation {
    pub driver: Id<Driver>,
    pub kind: HosEscalationKind,
    pub at: DateTime<Utc>,
    pub detail: String,
}

/// Errors raised when appending a duty interval to a [`HosLog`].
#[derive(Debug, thiserror::Error)]
pub enum HosLogError {
    /// The interval's status transition is illegal in the state graph.
    #[error(transparent)]
    Transition(#[from] HosStatusTransitionError),
    /// The interval is malformed (end strictly precedes start).
    #[error("invalid duty interval: end {end} precedes start {start}")]
    InvalidInterval {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
}

/// The live HOS enforcement engine: an append-only time-series of [`DutyInterval`]s that
/// recomputes every FMCSA-style limit deterministically and flags/escalates violations.
#[derive(Debug, Clone)]
pub struct HosLog {
    pub driver: Id<Driver>,
    pub status: HosStatus,
    pub intervals: Vec<DutyInterval>,
}

impl HosLog {
    /// A fresh, empty log for `driver` (currently off-duty).
    pub fn new(driver: Id<Driver>) -> Self {
        Self {
            driver,
            status: HosStatus::OffDuty,
            intervals: Vec::new(),
        }
    }

    /// The time up to which all limits are evaluated (end of the latest interval).
    pub fn as_of(&self) -> Option<DateTime<Utc>> {
        self.intervals.last().map(|iv| iv.end)
    }

    /// Append a sealed duty interval, rejecting illegal status transitions and malformed
    /// intervals. HOS rule breaches are *recorded* (via [`HosLog::violations`]/[`escalations`])
    /// rather than rejected, so the event series stays complete.
    pub fn append(&mut self, interval: DutyInterval) -> Result<(), HosLogError> {
        if interval.end < interval.start {
            return Err(HosLogError::InvalidInterval {
                start: interval.start,
                end: interval.end,
            });
        }
        // Each new interval must continue from the current status (state-graph enforced).
        self.status = self.status.transition(interval.status)?;
        self.intervals.push(interval);
        Ok(())
    }

    /// Cumulative driving and on-duty time within the current (post-reset) window, scanning
    /// back from the most recent interval until a qualifying 10h off-duty / 8h sleeper reset.
    fn scan_window(&self) -> (Duration, Duration) {
        let mut driving = Duration::ZERO;
        let mut on_duty = Duration::ZERO;
        for iv in self.intervals.iter().rev() {
            match iv.status {
                HosStatus::Driving => {
                    driving += iv.duration();
                    on_duty += iv.duration();
                }
                HosStatus::OnDuty => {
                    on_duty += iv.duration();
                }
                HosStatus::OffDuty => {
                    if iv.duration() >= H10_OFF {
                        break;
                    }
                }
                HosStatus::SleeperBerth => {
                    if iv.duration() >= H8_SLEEP {
                        break;
                    }
                }
            }
        }
        (driving, on_duty)
    }

    /// Cumulative driving within the current 11/14-hour window.
    pub fn driving_in_window(&self) -> Duration {
        self.scan_window().0
    }

    /// Cumulative on-duty within the current 14-hour window.
    pub fn on_duty_in_window(&self) -> Duration {
        self.scan_window().1
    }

    /// Cumulative driving since the most recent qualifying 30-minute break (off-duty or
    /// sleeper-berth). Returns total driving over the whole log when no break is recorded.
    pub fn driving_since_break(&self) -> Duration {
        let mut d = Duration::ZERO;
        for iv in self.intervals.iter().rev() {
            match iv.status {
                HosStatus::Driving => d += iv.duration(),
                HosStatus::OnDuty => {}
                HosStatus::OffDuty | HosStatus::SleeperBerth => {
                    if iv.duration() >= H30_BREAK {
                        return d;
                    }
                }
            }
        }
        d
    }

    /// Whether a 30-minute break is currently required (≥ 8h driving since last break).
    pub fn break_required(&self) -> bool {
        self.driving_since_break() >= H8_BREAK
    }

    /// On-duty time overlapping the rolling `days`-long window ending at [`as_of`].
    pub fn on_duty_in_days(&self, days: u32) -> Duration {
        let Some(as_of) = self.as_of() else {
            return Duration::ZERO;
        };
        let window_start = as_of - ChronoDuration::days(days as i64);
        let mut total = Duration::ZERO;
        for iv in &self.intervals {
            if !iv.status.is_on_duty() {
                continue;
            }
            let start = iv.start.max(window_start);
            let end = iv.end.min(as_of);
            if end > start {
                total += (end - start).to_std().unwrap_or(Duration::ZERO);
            }
        }
        total
    }

    /// The 60/70-hour cycle violation, if the rolling 7-day (60h) or 8-day (70h) window is
    /// exceeded. The 7-day limit is checked first as it is the tighter constraint.
    pub fn cycle_violation(&self) -> Option<HosViolation> {
        let in_7 = self.on_duty_in_days(7);
        if in_7 > H60 {
            return Some(HosViolation::CycleLimit {
                used: in_7,
                limit: H60,
                days: 7,
            });
        }
        let in_8 = self.on_duty_in_days(8);
        if in_8 > H70 {
            return Some(HosViolation::CycleLimit {
                used: in_8,
                limit: H70,
                days: 8,
            });
        }
        None
    }

    /// All current HOS violations against every enforced rule.
    pub fn violations(&self) -> Vec<HosViolation> {
        let mut v = Vec::new();
        let (drv, on) = self.scan_window();
        if drv > H11 {
            v.push(HosViolation::DrivingLimit(drv));
        }
        if on > H14 {
            v.push(HosViolation::OnDutyLimit(on));
        }
        if self.break_required() {
            v.push(HosViolation::BreakRequired(self.driving_since_break()));
        }
        if let Some(c) = self.cycle_violation() {
            v.push(c);
        }
        v
    }

    /// Whether the driver is currently within all HOS limits.
    pub fn is_compliant(&self) -> bool {
        self.violations().is_empty()
    }

    /// Dispatcher escalation records for every current violation, timestamped at [`as_of`].
    pub fn escalations(&self) -> Vec<HosEscalation> {
        let at = self.as_of().unwrap_or_else(Utc::now);
        self.violations()
            .into_iter()
            .map(|viol| {
                let (kind, detail) = match viol {
                    HosViolation::DrivingLimit(d) => (
                        HosEscalationKind::DrivingLimitExceeded,
                        format!("driving {d:?} exceeds 11h limit"),
                    ),
                    HosViolation::OnDutyLimit(d) => (
                        HosEscalationKind::OnDutyLimitExceeded,
                        format!("on-duty {d:?} exceeds 14h window"),
                    ),
                    HosViolation::BreakRequired(d) => (
                        HosEscalationKind::BreakRequired,
                        format!("{d:?} driving since last break; 30-min break required"),
                    ),
                    HosViolation::CycleLimit { used, limit, days } => (
                        HosEscalationKind::CycleLimitExceeded,
                        format!("{used:?} on-duty in {days}-day window exceeds {limit:?}"),
                    ),
                };
                HosEscalation {
                    driver: self.driver,
                    kind,
                    at,
                    detail,
                }
            })
            .collect()
    }

    /// Publish every current escalation to `tms.hos.escalation` on the given bus. Pure
    /// computation is in [`escalations`]; this is the only side-effecting step.
    pub async fn publish_escalations(
        &self,
        bus: &dyn EventBus,
    ) -> Result<(), tpt_erp_bus::BusError> {
        for esc in self.escalations() {
            let payload = serde_json::to_vec(&esc).expect("escalation serializes");
            bus.publish("tms.hos.escalation", &payload).await?;
        }
        Ok(())
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

    // ---- Live enforcement (HosLog) ----

    fn iv(status: HosStatus, start_h: i64, dur_h: u64) -> DutyInterval {
        let start = Utc::now() + ChronoDuration::hours(start_h);
        DutyInterval {
            status,
            start,
            end: start + ChronoDuration::hours(dur_h as i64),
        }
    }

    #[test]
    fn illegal_transition_rejected_by_state_machine() {
        let mut log = HosLog::new(Id::new());
        // First interval must be OnDuty (OffDuty -> OnDuty is legal); jumping to Driving
        // from OffDuty is rejected.
        assert!(log.append(iv(HosStatus::Driving, 0, 1)).is_err());
        log.append(iv(HosStatus::OnDuty, 0, 0)).unwrap();
        log.append(iv(HosStatus::Driving, 0, 1)).unwrap();
        // Driving -> OffDuty directly is illegal (must go via OnDuty).
        assert!(log.append(iv(HosStatus::OffDuty, 1, 1)).is_err());
    }

    #[test]
    fn driving_beyond_11h_flagged() {
        let mut log = HosLog::new(Id::new());
        log.append(iv(HosStatus::OnDuty, 0, 0)).unwrap();
        log.append(iv(HosStatus::Driving, 0, 12)).unwrap();
        assert!(!log.is_compliant());
        let v = log.violations();
        assert!(
            v.iter()
                .any(|x| matches!(x, HosViolation::DrivingLimit(d) if *d >= h(11)))
        );
    }

    #[test]
    fn on_duty_14h_window_enforced() {
        let mut log = HosLog::new(Id::new());
        log.append(iv(HosStatus::OnDuty, 0, 3)).unwrap();
        log.append(iv(HosStatus::Driving, 3, 11)).unwrap();
        log.append(iv(HosStatus::OnDuty, 14, 2)).unwrap();
        assert!(!log.is_compliant());
        assert!(
            log.violations()
                .iter()
                .any(|x| matches!(x, HosViolation::OnDutyLimit(d) if *d > H14))
        );
    }

    #[test]
    fn thirty_minute_break_required_after_8h_driving() {
        let mut log = HosLog::new(Id::new());
        log.append(iv(HosStatus::OnDuty, 0, 0)).unwrap();
        // 8h driving with no break -> break required.
        log.append(iv(HosStatus::Driving, 0, 8)).unwrap();
        assert!(log.break_required());
        assert!(
            log.violations()
                .iter()
                .any(|x| matches!(x, HosViolation::BreakRequired(_)))
        );
        // A 30-min off-duty break resets the since-break counter.
        log.append(iv(HosStatus::OnDuty, 8, 0)).unwrap();
        log.append(iv(HosStatus::OffDuty, 8, 1)).unwrap();
        assert!(!log.break_required());
        // Another 8h driving after the break re-triggers the requirement.
        log.append(iv(HosStatus::OnDuty, 9, 0)).unwrap();
        log.append(iv(HosStatus::Driving, 9, 8)).unwrap();
        assert!(log.break_required());
    }

    #[test]
    fn cycle_limit_60_in_7_day_rolling_window() {
        let mut log = HosLog::new(Id::new());
        // 7 consecutive days of 9h on-duty each = 63h, exceeding the 60h/7-day cycle.
        // A 10h off-duty rest between days resets the daily 11/14h window but the cycle
        // still accumulates across the rolling 7-day window.
        for day in 0..7i64 {
            log.append(iv(HosStatus::OnDuty, day * 24, 9)).unwrap();
            log.append(iv(HosStatus::OffDuty, day * 24 + 9, 10))
                .unwrap();
        }
        assert_eq!(log.on_duty_in_days(7), h(63));
        let v = log.cycle_violation().expect("expect 7-day cycle violation");
        match v {
            HosViolation::CycleLimit { used, limit, days } => {
                assert_eq!(days, 7);
                assert_eq!(used, h(63));
                assert_eq!(limit, H60);
            }
            _ => panic!("unexpected violation"),
        }
    }

    #[test]
    fn cycle_limit_within_70_in_8_days_is_compliant() {
        let mut log = HosLog::new(Id::new());
        // 8 consecutive days of 8h on-duty each = 64h, under both 60/7 and 70/8 limits.
        // A 10h off-duty rest between days resets the daily 11/14h window.
        for day in 0..8i64 {
            log.append(iv(HosStatus::OnDuty, day * 24, 8)).unwrap();
            log.append(iv(HosStatus::OffDuty, day * 24 + 8, 10))
                .unwrap();
        }
        assert!(log.cycle_violation().is_none());
        assert!(log.is_compliant());
    }

    #[test]
    fn violation_triggers_escalation_record() {
        let mut log = HosLog::new(Id::new());
        log.append(iv(HosStatus::OnDuty, 0, 0)).unwrap();
        log.append(iv(HosStatus::Driving, 0, 12)).unwrap();
        let esc = log.escalations();
        assert!(!esc.is_empty());
        assert!(
            esc.iter()
                .any(|e| e.kind == HosEscalationKind::DrivingLimitExceeded)
        );
        // Escalation records serialize (and thus can be published on the bus).
        let json = serde_json::to_string(&esc[0]).unwrap();
        let back: HosEscalation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, esc[0]);
    }

    #[tokio::test]
    async fn publishes_escalations_to_bus() {
        use futures::StreamExt as _;
        use tpt_erp_bus::memory::InMemoryBus;
        let bus = InMemoryBus::new();
        let mut sub = bus.subscribe("tms.hos.escalation").await.unwrap();

        let mut log = HosLog::new(Id::new());
        log.append(iv(HosStatus::OnDuty, 0, 0)).unwrap();
        log.append(iv(HosStatus::Driving, 0, 12)).unwrap();
        log.publish_escalations(&bus).await.unwrap();

        let msg = sub.next().await.expect("escalation published");
        let esc: HosEscalation = serde_json::from_slice(&msg.payload).unwrap();
        assert_eq!(esc.kind, HosEscalationKind::DrivingLimitExceeded);
    }
}
