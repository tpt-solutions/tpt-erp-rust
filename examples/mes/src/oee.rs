//! Real-time OEE (Overall Equipment Effectiveness) calculation.
//!
//! OEE is the product of three factors, each in `[0, 1]`:
//!
//! - **Availability** = run time / planned production time
//! - **Performance**  = (ideal cycle time × total count) / run time
//! - **Quality**      = good count / total count
//!
//! `OEE = Availability × Performance × Quality`. World-class is ~85%; typical is ~60%.

/// One production run's raw inputs for an OEE calculation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProductionRun {
    /// Scheduled production time (seconds), including planned stops.
    pub planned_time_secs: f64,
    /// Actual operating (run) time (seconds), excluding breakdowns/stops.
    pub run_time_secs: f64,
    /// Ideal seconds to produce one unit on this machine.
    pub ideal_cycle_time_secs: f64,
    /// Total units produced (good + defective).
    pub total_count: u64,
    /// Units that passed quality.
    pub good_count: u64,
}

/// The computed OEE breakdown. All factors are clamped to `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oee {
    pub availability: f64,
    pub performance: f64,
    pub quality: f64,
    /// `availability × performance × quality`.
    pub oee: f64,
}

fn clamp01(x: f64) -> f64 {
    if !x.is_finite() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

impl ProductionRun {
    /// Compute the OEE breakdown for this run.
    pub fn oee(&self) -> Oee {
        let availability = clamp01(self.run_time_secs / self.planned_time_secs);
        let performance =
            clamp01((self.ideal_cycle_time_secs * self.total_count as f64) / self.run_time_secs);
        let quality = clamp01(self.good_count as f64 / self.total_count as f64);
        Oee {
            availability,
            performance,
            quality,
            oee: availability * performance * quality,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_run_is_full_oee() {
        let run = ProductionRun {
            planned_time_secs: 100.0,
            run_time_secs: 100.0,
            ideal_cycle_time_secs: 1.0,
            total_count: 100,
            good_count: 100,
        };
        let oee = run.oee();
        assert!((oee.oee - 1.0).abs() < 1e-9);
        assert!((oee.availability - 1.0).abs() < 1e-9);
        assert!((oee.performance - 1.0).abs() < 1e-9);
        assert!((oee.quality - 1.0).abs() < 1e-9);
    }

    #[test]
    fn typical_world_class_is_around_85_percent() {
        // Availability 432/480 = 0.90, Performance 410/432 ~= 0.9491,
        // Quality 406/410 ~= 0.9902 -> OEE ~= 0.8467.
        let run = ProductionRun {
            planned_time_secs: 480.0,
            run_time_secs: 432.0, // 90% availability
            ideal_cycle_time_secs: 1.0,
            total_count: 410, // 410 units in 432s
            good_count: 406,  // 406 good of 410
        };
        let oee = run.oee();
        assert!((oee.availability - 0.90).abs() < 1e-9);
        assert!((oee.performance - 410.0 / 432.0).abs() < 1e-9);
        assert!((oee.quality - 406.0 / 410.0).abs() < 1e-9);
        assert!((oee.oee - 0.8458333).abs() < 1e-4);
    }

    #[test]
    fn defects_drive_quality_down() {
        let run = ProductionRun {
            planned_time_secs: 100.0,
            run_time_secs: 100.0,
            ideal_cycle_time_secs: 1.0,
            total_count: 100,
            good_count: 90,
        };
        assert!((run.oee().quality - 0.90).abs() < 1e-9);
        assert!((run.oee().oee - 0.90).abs() < 1e-9);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let run = ProductionRun {
            planned_time_secs: 0.0,
            run_time_secs: 0.0,
            ideal_cycle_time_secs: 1.0,
            total_count: 0,
            good_count: 0,
        };
        let oee = run.oee();
        assert_eq!(oee.oee, 0.0);
    }
}
