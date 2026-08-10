//! Machine telemetry ingestion and live OEE.
//!
//! CNC/PLC machines emit high-frequency samples (cycle completions, run-state changes,
//! spindle/temperature readings). This module ingests those samples into a per-machine
//! accumulator and computes live OEE (see [`crate::oee`]) without a time-series database —
//! the [`TelemetryStore`] trait isolates storage so a TimescaleDB backend can be dropped in
//! later without touching the aggregation logic.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;

/// A single machine telemetry sample.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetrySample {
    /// A produced unit. `good` distinguishes conforming from scrap.
    Cycle { machine: String, good: bool },
    /// Operating (run) time advanced by `seconds`.
    RunTime { machine: String, seconds: f64 },
}

#[derive(Debug, Deserialize)]
struct WireSample {
    machine: String,
    #[serde(rename = "type")]
    kind: String,
    good: Option<bool>,
    seconds: Option<f64>,
}

/// Decode a JSON telemetry frame. Returns `None` for unparseable frames.
pub fn decode_telemetry(payload: &[u8]) -> Option<TelemetrySample> {
    let w: WireSample = serde_json::from_slice(payload).ok()?;
    match w.kind.as_str() {
        "cycle" => Some(TelemetrySample::Cycle {
            machine: w.machine,
            good: w.good.unwrap_or(true),
        }),
        "runtime" => Some(TelemetrySample::RunTime {
            machine: w.machine,
            seconds: w.seconds.unwrap_or(0.0),
        }),
        _ => None,
    }
}

/// Accumulated production state for one machine.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MachineState {
    pub run_time_secs: f64,
    pub total_count: u64,
    pub good_count: u64,
}

/// Static configuration needed to turn a [`MachineState`] into OEE.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MachineConfig {
    pub planned_time_secs: f64,
    pub ideal_cycle_time_secs: f64,
}

/// Storage for telemetry accumulators. Implemented by [`InMemoryTelemetryStore`]
/// and (later) a TimescaleDB-backed store.
pub trait TelemetryStore: Send + Sync {
    /// Fold a sample into a machine's accumulator.
    fn record(&self, sample: &TelemetrySample);
    /// Current accumulator for a machine, if any samples arrived.
    fn state(&self, machine: &str) -> Option<MachineState>;
    /// Live OEE for a machine given its config, or `None` if unknown.
    fn oee(&self, machine: &str, config: MachineConfig) -> Option<crate::oee::Oee>;
}

/// In-process telemetry store: a `Mutex<HashMap>` of per-machine accumulators.
#[derive(Default)]
pub struct InMemoryTelemetryStore {
    states: Mutex<HashMap<String, MachineState>>,
}

impl InMemoryTelemetryStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TelemetryStore for InMemoryTelemetryStore {
    fn record(&self, sample: &TelemetrySample) {
        let mut states = self.states.lock().unwrap();
        let s = states.entry(match sample {
            TelemetrySample::Cycle { machine, .. } => machine.clone(),
            TelemetrySample::RunTime { machine, .. } => machine.clone(),
        })
        .or_default();
        match sample {
            TelemetrySample::Cycle { good, .. } => {
                s.total_count += 1;
                if *good {
                    s.good_count += 1;
                }
            }
            TelemetrySample::RunTime { seconds, .. } => {
                s.run_time_secs += *seconds;
            }
        }
    }

    fn state(&self, machine: &str) -> Option<MachineState> {
        self.states.lock().unwrap().get(machine).copied()
    }

    fn oee(&self, machine: &str, config: MachineConfig) -> Option<crate::oee::Oee> {
        let s = self.state(machine)?;
        let run = crate::oee::ProductionRun {
            planned_time_secs: config.planned_time_secs,
            run_time_secs: s.run_time_secs,
            ideal_cycle_time_secs: config.ideal_cycle_time_secs,
            total_count: s.total_count,
            good_count: s.good_count,
        };
        Some(run.oee())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        // 432s run time, 410 cycles (406 good), on a 480s planned window, 1s cycle.
        v.push(br#"{"machine":"M1","type":"runtime","seconds":432.0}"#.to_vec());
        for i in 0..410 {
            let good = i < 406;
            v.push(
                serde_json::json!({"machine":"M1","type":"cycle","good":good})
                    .to_string()
                    .into_bytes(),
            );
        }
        v
    }

    #[test]
    fn decode_handles_cycle_and_runtime() {
        let c = decode_telemetry(br#"{"machine":"M1","type":"cycle","good":false}"#).unwrap();
        assert_eq!(c, TelemetrySample::Cycle { machine: "M1".into(), good: false });
        let r = decode_telemetry(br#"{"machine":"M1","type":"runtime","seconds":5}"#).unwrap();
        assert_eq!(r, TelemetrySample::RunTime { machine: "M1".into(), seconds: 5.0 });
        assert!(decode_telemetry(b"junk").is_none());
    }

    #[test]
    fn store_accumulates_and_computes_oee() {
        let store = InMemoryTelemetryStore::new();
        for f in frames() {
            if let Some(s) = decode_telemetry(&f) {
                store.record(&s);
            }
        }
        let cfg = MachineConfig {
            planned_time_secs: 480.0,
            ideal_cycle_time_secs: 1.0,
        };
        let oee = store.oee("M1", cfg).unwrap();
        assert!((oee.availability - 0.90).abs() < 1e-9, "avail {}", oee.availability);
        assert!((oee.performance - 410.0 / 432.0).abs() < 1e-9, "perf {}", oee.performance);
        assert!((oee.quality - 406.0 / 410.0).abs() < 1e-9, "qual {}", oee.quality);
        assert!((oee.oee - 0.8458333).abs() < 1e-4, "oee {}", oee.oee);
    }

    #[test]
    fn unknown_machine_has_no_oee() {
        let store = InMemoryTelemetryStore::new();
        let cfg = MachineConfig {
            planned_time_secs: 480.0,
            ideal_cycle_time_secs: 1.0,
        };
        assert!(store.oee("ghost", cfg).is_none());
    }
}
