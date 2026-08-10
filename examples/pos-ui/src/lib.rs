//! Leptos cashier-terminal view for the POS reference ERP.
//!
//! A **front-end** mirror of the register. It re-uses the strong-ID types from
//! [`tpt_erp_primitives`] so the same `Id` protections that guard the event-sourced
//! sale log on the server also guard the cashier's screen.
//!
//! The transaction pipeline [`TxnStage`] is a faithful re-implementation of the
//! server's [`pos::txn::TxnStatus`] state machine, used to render live **status
//! badges** as a sale moves through `Cart → Tendering → Authorized → Captured`. An
//! **offline/online indicator** binds to a connectivity signal so the cashier sees
//! whether sales are being recorded locally only (pending sync) or reconciled.

use leptos::prelude::*;
use tpt_erp_primitives::{Entity, Id};

/// Marker entity for a register item (mirrors `pos::txn::PosItem` typing).
#[derive(Debug)]
pub struct Item;
impl Entity for Item {}

/// A live line on the cashier's transaction grid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LineRow {
    id: Id<Item>,
    name: &'static str,
    qty: u32,
    price_cents: i64,
}

/// The lifecycle of a sale, mirroring the server's `TxnStatus` state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
enum TxnStage {
    Cart,
    Tendering,
    Authorized,
    Captured,
    Voided,
    Refunded,
}

impl TxnStage {
    /// The ordered pipeline used to render the badge trail.
    fn pipeline() -> &'static [TxnStage] {
        &[
            TxnStage::Cart,
            TxnStage::Tendering,
            TxnStage::Authorized,
            TxnStage::Captured,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            TxnStage::Cart => "Cart",
            TxnStage::Tendering => "Tendering",
            TxnStage::Authorized => "Authorized",
            TxnStage::Captured => "Captured",
            TxnStage::Voided => "Voided",
            TxnStage::Refunded => "Refunded",
        }
    }
}

/// A badge in the sale pipeline; `active` when it is the current step.
#[component]
fn StageBadge(stage: TxnStage, current: Signal<TxnStage>) -> impl IntoView {
    let cls = move || {
        let cur = current.get();
        if matches!(stage, TxnStage::Voided | TxnStage::Refunded) {
            "badge bad"
        } else if stage == cur {
            "badge active"
        } else {
            "badge"
        }
    };
    view! { <span class=cls>{stage.label()}</span> }
}

/// The cashier terminal: a transaction grid, a live sale-status badge trail, and an
/// offline/online connectivity indicator.
#[component]
pub fn App() -> impl IntoView {
    let lines = RwSignal::new(vec![
        RwSignal::new(LineRow { id: Id::new(), name: "House Blend", qty: 2, price_cents: 650 }),
        RwSignal::new(LineRow { id: Id::new(), name: "Croissant", qty: 1, price_cents: 425 }),
        RwSignal::new(LineRow { id: Id::new(), name: "Sparkling", qty: 3, price_cents: 300 }),
    ]);

    let current = RwSignal::new(TxnStage::Cart);
    let advance = move |_| {
        let next = match current.get() {
            TxnStage::Cart => TxnStage::Tendering,
            TxnStage::Tendering => TxnStage::Authorized,
            TxnStage::Authorized => TxnStage::Captured,
            TxnStage::Captured | TxnStage::Voided | TxnStage::Refunded => TxnStage::Captured,
        };
        current.set(next);
    };

    // Connectivity: in production this reflects the register's link to the central
    // store; here it is a local signal the UI binds to so the indicator updates live.
    let online = RwSignal::new(true);
    let toggle_conn = move |_| online.set(!online.get());

    let badges = RwSignal::new(TxnStage::pipeline().to_vec());

    view! {
        <main>
            <h1>"TPT ERP — Cashier Terminal"</h1>

            <section class="conn">
                <span
                    class:online=move || online.get()
                    class:offline=move || !online.get()
                >
                    {move || if online.get() { "● Online" } else { "● Offline — recording locally" }}
                </span>
                <button on:click=toggle_conn>"Toggle connection"</button>
            </section>

            <p class="muted">"Retail / POS reference UI. Type-safe item ids via tpt-erp-primitives."</p>

            <section>
                <h2>"Current Transaction"</h2>
                <table>
                    <thead>
                        <tr><th>"Item"</th><th>"Qty"</th><th>"Price"</th><th>"SKU"</th></tr>
                    </thead>
                    <tbody>
                        <For
                            each=move || lines.get()
                            key=|l| l.get().id
                            let:item
                        >
                            <tr>
                                <td>{move || item.get().name}</td>
                                <td>{move || item.get().qty}</td>
                                <td>{move || format!("${:.2}", item.get().price_cents as f64 / 100.0)}</td>
                                <td><span class="pill">{move || item.get().id.as_str()}</span></td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            </section>

            <section>
                <h2>"Sale Status"</h2>
                <div class="badges">
                    <For
                        each=move || badges.get()
                        key=|s| *s
                        let:item
                    >
                        <StageBadge stage=item current=current.into() />
                    </For>
                </div>
                <button on:click=advance>"Advance sale →"</button>
            </section>
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
