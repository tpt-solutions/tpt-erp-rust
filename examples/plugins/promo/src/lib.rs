//! Example TPT ERP plugin: a stock-aware promo / discount engine.
//!
//! The plugin *reads* live stock levels from the host via the `erp` interface and
//! returns a per-SKU, stock-aware discount (clearance pricing when stock is high,
//! scarcity pricing when it is low). It performs no I/O of its own — the host never
//! links WASI, so this guest is fully sandboxed.

wit_bindgen::generate!({ world: "plugin" });

use crate::tpt::erp::erp::get_stock_level;
use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {
    fn run(input: String) -> Result<String, String> {
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let sku = v["sku"].as_str().unwrap_or("").to_string();
        let qty = v["qty"].as_u64().unwrap_or(0);
        // Unit price arrives in minor units (cents) to stay integer-exact across the
        // host/guest boundary.
        let unit_price = v["unit_price"].as_i64().unwrap_or(0);

        // Read live stock from the host; treat an unknown SKU as zero stock.
        let stock = match get_stock_level(&sku) {
            Ok(s) => s as i64,
            Err(_) => 0i64,
        };

        // Stock-aware discount ladder: plenty of stock → clearance discount;
        // scarce stock → little or no discount. Bulk quantities add a small extra.
        let mut pct = if stock >= 100 {
            15
        } else if stock >= 20 {
            5
        } else {
            0
        };
        if qty >= 5 {
            pct += 5;
        }

        let discount = unit_price * pct / 100;
        let final_price = (unit_price - discount).max(0);

        let out = serde_json::json!({
            "sku": sku,
            "unit_price": unit_price,
            "stock": stock,
            "discount_pct": pct,
            "final_price": final_price,
        });
        Ok(out.to_string())
    }
}

export!(Component);
