//! Example TPT ERP plugin: a dispatch (stop-scoring) engine.
//!
//! Demonstrates the computation-only contract end-to-end: the plugin *reads* a stop's
//! demand from the host via the `erp` interface (`get-stock-level`, reused as the
//! demand channel) and returns a dispatch priority score that blends demand with the
//! routing weight (e.g. distance). It performs no I/O of its own — the host never links
//! WASI, so this guest is fully sandboxed.

wit_bindgen::generate!({ world: "plugin" });

use crate::tpt::erp::erp::get_stock_level;
use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {
    fn run(input: String) -> Result<String, String> {
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let stop = v["stop"].as_str().unwrap_or("").to_string();
        let weight = v["weight"].as_i64().unwrap_or(0);

        // Read the stop's demand from the host. The plugin can only *read* data the
        // host chooses to expose; it cannot write or reach the network.
        let demand = match get_stock_level(&stop) {
            Ok(d) => d,
            Err(_) => 0,
        };

        // Priority score: higher demand pushes the stop earlier; longer routing weight
        // (distance) discounts it. Clamped to be non-negative.
        let raw = (demand as i64) * 10 - weight / 100;
        let score = if raw < 0 { 0 } else { raw };

        let out = serde_json::json!({
            "stop": stop,
            "weight": weight,
            "demand": demand,
            "score": score,
        });
        Ok(out.to_string())
    }
}

export!(Component);
