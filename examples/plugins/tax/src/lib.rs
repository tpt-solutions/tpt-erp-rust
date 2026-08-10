//! Example TPT ERP plugin: jurisdiction tax-tier engine.
//!
//! Reads an account's balance from the host via `erp` and decides a sales-tax tier for
//! the given jurisdiction (e.g. EU reduced vs standard vs exempt). It performs no I/O
//! of its own — the host never links WASI, so the plugin is computation-only by
//! construction and can be hot-swapped per client without a server restart.

wit_bindgen::generate!({ world: "plugin" });

use crate::tpt::erp::erp::get_account_balance;
use serde_json::Value;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

struct Component;

impl Guest for Component {
    fn run(input: String) -> Result<String, String> {
        let v: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        let account = v["account"].as_str().unwrap_or("").to_string();
        let jurisdiction = v["jurisdiction"].as_str().unwrap_or("default").to_string();

        // Read the account balance (major + 1/10_000ths minor) from the host.
        let (major, minor) = match get_account_balance(&account) {
            Ok(m) => (m.major, m.minor),
            Err(_) => (0, 0),
        };
        let balance = major as f64 + minor as f64 / 10_000.0;

        // Jurisdiction-specific tax tiers on the taxable base.
        let (tier, rate) = match jurisdiction.as_str() {
            "eu-reduced" => ("reduced", 0.09),
            "eu-standard" => ("standard", 0.21),
            "exempt" => ("exempt", 0.0),
            _ => ("standard", 0.20),
        };
        let estimated_tax = if rate > 0.0 { balance * rate } else { 0.0 };

        let out = serde_json::json!({
            "account": account,
            "jurisdiction": jurisdiction,
            "balance": balance,
            "tax_tier": tier,
            "tax_rate": rate,
            "estimated_tax": estimated_tax,
        });
        Ok(out.to_string())
    }
}

export!(Component);
