//! Leptos frontend for TPT ERP.
//!
//! This crate is compiled to WebAssembly (`wasm32-unknown-unknown`) and mounts a
//! small UI. It deliberately re-uses [`tpt_erp_primitives::Money`] so the *same*
//! type-safe money type that protects the ledger on the server also protects the
//! shopping cart in the browser — one definition, zero drift.
//!
//! Build & serve:
//!
//! ```sh
//! cargo install trunk
//! trunk build --release
//! ```

use leptos::prelude::*;
use tpt_erp_primitives::{Money, Usd};

/// A strongly-typed line total computed in the browser with the shared `Money`
/// type. Because `Money<Usd>` is the same struct the backend uses, a currency
/// mistake is a compile error here exactly as it is server-side.
fn line_total(unit: Money<Usd>, qty: u32) -> Money<Usd> {
    unit * rust_decimal::Decimal::from(qty)
}

/// Root component.
#[component]
pub fn App() -> impl IntoView {
    let unit_price = Money::<Usd>::from_major(19);
    let qty = 3u32;
    let total = line_total(unit_price, qty);

    view! {
        <main>
            <h1>"TPT ERP — storefront"</h1>
            <p>{format!("Unit price: {unit_price}")}</p>
            <p>{format!("Quantity: {qty}")}</p>
            <p>{format!("Total: {total}")}</p>
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
