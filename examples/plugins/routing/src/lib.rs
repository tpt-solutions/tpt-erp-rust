//! Example TPT ERP plugin: a wave/zone routing engine.
//!
//! Demonstrates the computation-only contract for the WMS: the plugin *reads*
//! on-hand stock for each SKU in a pick list from the host via `erp` and decides a
//! picker-routing strategy (Zone Picking when stock is concentrated in one zone, else
//! Wave Picking). It performs no I/O of its own — the host never links WASI.

wit_bindgen::generate!({ world: "plugin" });

use crate::tpt::erp::erp::get_stock_level;
use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {
    fn run(input: String) -> Result<String, String> {
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let skus = v["skus"].as_array().cloned().unwrap_or_default();

        // Read host stock for each SKU and bucket it into one of 4 warehouse zones
        // (zone derived deterministically from the SKU string).
        let mut zones: std::collections::HashMap<usize, i64> = std::collections::HashMap::new();
        for s in &skus {
            let sku = s.as_str().unwrap_or("").to_string();
            let level = match get_stock_level(&sku) {
                Ok(l) => l as i64,
                Err(_) => 0,
            };
            let zone = (sku.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32)) % 4) as usize;
            *zones.entry(zone).or_insert(0) += level;
        }

        let total: i64 = zones.values().sum();
        // Stock concentrated in a single zone -> Zone Picking; spread out -> Wave Picking.
        let strategy = if zones.values().any(|&c| c * 2 >= total.max(1)) {
            "zone"
        } else {
            "wave"
        };

        let out = serde_json::json!({
            "strategy": strategy,
            "zones": zones,
            "total_stock": total,
        });
        Ok(out.to_string())
    }
}

export!(Component);
