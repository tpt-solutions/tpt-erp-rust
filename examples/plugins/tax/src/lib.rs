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
        // Represent the balance in integer minor units (1/10_000ths) to avoid any `f64`
        // drift; tax is computed in basis points, also as integers.
        let balance_minor = major as i64 * 10_000 + minor as i64;

        // Jurisdiction-specific tax tiers (basis points of the taxable base).
        let (tier, basis_points) = match jurisdiction.as_str() {
            "eu-reduced" => ("reduced", 900),
            "eu-standard" => ("standard", 2100),
            "exempt" => ("exempt", 0),
            _ => ("standard", 2000),
        };
        let tax_minor = if basis_points > 0 {
            balance_minor * basis_points / 10_000
        } else {
            0
        };

        let out = serde_json::json!({
            "account": account,
            "jurisdiction": jurisdiction,
            "balance": format!("{}.{:04}", balance_minor / 10_000, balance_minor % 10_000),
            "tax_tier": tier,
            "tax_rate_bps": basis_points,
            "estimated_tax": format!("{}.{:04}", tax_minor / 10_000, tax_minor % 10_000),
        });
        Ok(out.to_string())
    }
}

export!(Component);
