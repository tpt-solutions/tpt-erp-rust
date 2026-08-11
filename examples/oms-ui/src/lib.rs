//! Leptos storefront/checkout view for the OMS reference ERP.
//!
//! This is a **front-end** mirror of the storefront. It re-uses the strong-ID types
//! from [`tpt_erp_primitives`] so the same `Id<Sku>`/`Id<Product>` that protect the
//! event-sourced reservation engine on the server also protect the customer's screen —
//! a currency-style mix-up of two SKUs is a compile error here.
//!
//! The order lifecycle [`SagaStatus`] is a faithful re-implementation of the server's
//! `OrderStatus` state machine, used to render live **saga-status badges** as an order
//! moves through `Cart → Reserved → Paid → Fulfilled → Shipped`.

use leptos::prelude::*;
use tpt_erp_primitives::{Entity, Id};

/// Marker entity for a catalog product (mirrors `oms::reservation::Sku` typing).
#[derive(Debug)]
pub struct Product;
impl Entity for Product {}

/// A live product row in the storefront grid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ProductRow {
    id: Id<Product>,
    name: &'static str,
    price_cents: i64,
    stock: i64,
}

/// The lifecycle of an order, mirroring the server's `OrderStatus` state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SagaStatus {
    Cart,
    Reserved,
    Paid,
    Fulfilled,
    Shipped,
    Cancelled,
}

impl SagaStatus {
    /// The ordered pipeline used to render the badge trail.
    fn pipeline() -> &'static [SagaStatus] {
        &[
            SagaStatus::Cart,
            SagaStatus::Reserved,
            SagaStatus::Paid,
            SagaStatus::Fulfilled,
            SagaStatus::Shipped,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            SagaStatus::Cart => "Cart",
            SagaStatus::Reserved => "Reserved",
            SagaStatus::Paid => "Paid",
            SagaStatus::Fulfilled => "Fulfilled",
            SagaStatus::Shipped => "Shipped",
            SagaStatus::Cancelled => "Cancelled",
        }
    }
}

/// A badge in the order pipeline; `active` when it is the current step.
#[component]
fn StatusBadge(status: SagaStatus, current: Signal<SagaStatus>) -> impl IntoView {
    let cls = move || {
        let cur = current.get();
        if status == SagaStatus::Cancelled {
            "badge bad"
        } else if status == cur {
            "badge active"
        } else {
            "badge"
        }
    };
    view! { <span class=cls>{status.label()}</span> }
}

/// The storefront: a product grid plus a live saga-status badge trail.
#[component]
pub fn App() -> impl IntoView {
    let products = RwSignal::new(vec![
        RwSignal::new(ProductRow {
            id: Id::new(),
            name: "Trail Runner",
            price_cents: 12900,
            stock: 12,
        }),
        RwSignal::new(ProductRow {
            id: Id::new(),
            name: "Daypack 22L",
            price_cents: 8900,
            stock: 3,
        }),
        RwSignal::new(ProductRow {
            id: Id::new(),
            name: "Merino Tee",
            price_cents: 3900,
            stock: 140,
        }),
    ]);

    // Order saga status is driven by the server in production; here it is a local
    // signal the UI binds to so the badge trail updates live as the saga advances.
    let current = RwSignal::new(SagaStatus::Cart);
    let advance = move |_| {
        let next = match current.get() {
            SagaStatus::Cart => SagaStatus::Reserved,
            SagaStatus::Reserved => SagaStatus::Paid,
            SagaStatus::Paid => SagaStatus::Fulfilled,
            SagaStatus::Fulfilled => SagaStatus::Shipped,
            SagaStatus::Shipped | SagaStatus::Cancelled => SagaStatus::Shipped,
        };
        current.set(next);
    };

    let badges = RwSignal::new(SagaStatus::pipeline().to_vec());

    view! {
        <main>
            <h1>"TPT ERP — Storefront"</h1>
            <p class="muted">"E-commerce / OMS reference UI. Type-safe product ids via tpt-erp-primitives."</p>

            <section>
                <h2>"Catalog"</h2>
                <table>
                    <thead>
                        <tr><th>"Product"</th><th>"Price"</th><th>"Stock"</th><th>"SKU"</th></tr>
                    </thead>
                    <tbody>
                        <For
                            each=move || products.get()
                            key=|p| p.get().id
                            let:item
                        >
                            <tr>
                                <td>{move || item.get().name}</td>
                                <td>{move || format!("${:.2}", item.get().price_cents as f64 / 100.0)}</td>
                                <td class:warn=move || item.get().stock < 10>{move || item.get().stock}</td>
                                <td><span class="pill">{move || item.get().id.as_str()}</span></td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            </section>

            <section>
                <h2>"Checkout"</h2>
                <p class="muted">"Live order saga status:"</p>
                <div class="badges">
                    <For
                        each=move || badges.get()
                        key=|s| *s
                        let:item
                    >
                        <StatusBadge status=item current=current.into() />
                    </For>
                </div>
                <button on:click=advance>"Advance saga →"</button>
            </section>
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
