//! Example TPT ERP plugin: QC tolerance check + telemetry parser.
//!
//! Demonstrates the computation-only contract for the MES: the plugin performs two
//! edge-node evaluations with no host I/O:
//!
//! - `qc`     — a dimensional tolerance check (pass/fail against a nominal ± tolerance).
//! - `telemetry` — parse a raw CNC/PLC line (`M1,spindle,1234.5`) into structured JSON.
//!
//! Both run in microseconds and can be swapped per client at the edge without a
//! server restart (see `tpt-wasm`'s `PluginHandle::swap_module`).

wit_bindgen::generate!({ world: "plugin" });

use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {
    fn run(input: String) -> Result<String, String> {
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let kind = v["type"].as_str().unwrap_or("");
        match kind {
            "qc" => {
                let nominal = v["nominal"].as_f64().unwrap_or(0.0);
                let measured = v["measured"].as_f64().unwrap_or(0.0);
                let tol = v["tol"].as_f64().unwrap_or(0.0);
                let deviation = (measured - nominal).abs();
                let pass = deviation <= tol;
                let out = serde_json::json!({
                    "pass": pass,
                    "deviation": deviation,
                    "nominal": nominal,
                    "measured": measured,
                    "tolerance": tol,
                });
                Ok(out.to_string())
            }
            "telemetry" => {
                // Parse a raw machine line into structured form.
                let raw = v["raw"].as_str().unwrap_or("");
                let parts: Vec<&str> = raw.split(',').collect();
                let parsed = if parts.len() >= 3 {
                    serde_json::json!({
                        "machine": parts[0],
                        "signal": parts[1],
                        "value": parts[2].parse::<f64>().unwrap_or(0.0),
                    })
                } else {
                    serde_json::json!({ "error": "malformed telemetry frame" })
                };
                Ok(parsed.to_string())
            }
            other => Err(format!("unknown qc/telemetry type: {other}")),
        }
    }
}

export!(Component);
