//! Example TPT ERP plugin: a pricing engine.
//!
//! Demonstrates the computation-only contract end-to-end: the plugin
//! *reads* an account balance from the host via the `erp` interface and
//! applies a balance-tiered discount. It performs no I/O of its own — the
//! host never links WASI, so this guest is fully sandboxed.

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
        let amount = v["amount"].as_i64().unwrap_or(0);

        // Read the account balance from the host. The plugin can only
        // *read* data the host chooses to expose; it cannot write or
        // reach the network.
        let balance = match get_account_balance(&account) {
            Ok(b) => b.major * 10_000 + b.minor,
            Err(_) => 0,
        };

        // Balance-tiered discount: bigger balances get a bigger break.
        let discount_pct = if balance > 1_000_000 { 10 } else { 2 };
        let final_amount = amount - (amount * discount_pct / 100);

        let out = serde_json::json!({
            "account": account,
            "amount": amount,
            "balance": balance,
            "discount_pct": discount_pct,
            "final_amount": final_amount,
        });
        Ok(out.to_string())
    }
}

export!(Component);
