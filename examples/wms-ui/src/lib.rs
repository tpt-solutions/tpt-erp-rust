//! Leptos operator UI for the WMS reference ERP.
//!
//! This is a **front-end** mirror of the warehouse operator view. It re-uses the
//! strong-ID types from [`tpt_erp_primitives`] so the same `Id<Bin>`/`Id<Sku>` that
//! protect the event-sourced inventory engine on the server also protect the
//! picker's screen - a currency-style mix-up of two bins is a compile error here.
//!
//! The route-planning math is a faithful, dependency-light re-implementation of
//! `examples/wms/src/picking.rs` so the operator can preview the pick path without
//! a server round-trip. (The server remains the source of truth for inventory.)

use leptos::prelude::*;
use tpt_erp_primitives::{Entity, Id};

/// Marker entity for a physical warehouse bin.
#[derive(Debug)]
pub struct Bin;
impl Entity for Bin {}

/// Marker entity for a stock-keeping unit.
#[derive(Debug)]
pub struct Sku;
impl Entity for Sku {}

/// A row in the operator's on-hand inventory grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StockRow {
    bin: Id<Bin>,
    sku: Id<Sku>,
    qty: i64,
}

/// A pick location in the warehouse grid (aisle `x`, position `y` along aisle).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PickLoc {
    id: Id<Bin>,
    x: i32,
    y: i32,
}

// ----------------------------------------------------------------------------
// Picker-route math (mirrors `examples/wms/src/picking.rs`).
// ----------------------------------------------------------------------------

fn dist(ax: i32, ay: i32, bx: i32, by: i32) -> f64 {
    let dx = (ax - bx) as f64;
    let dy = (ay - by) as f64;
    (dx * dx + dy * dy).sqrt()
}

fn route_distance(locs: &[PickLoc], order: &[usize]) -> f64 {
    let mut total = 0.0;
    let (mut cx, mut cy) = (0, 0);
    for &i in order {
        let l = &locs[i];
        total += dist(cx, cy, l.x, l.y);
        cx = l.x;
        cy = l.y;
    }
    total + dist(cx, cy, 0, 0)
}

fn naive_route(n: usize) -> Vec<usize> {
    (0..n).collect()
}

fn nearest_neighbor_route(locs: &[PickLoc]) -> Vec<usize> {
    let n = locs.len();
    let mut visited = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let (mut cx, mut cy) = (0, 0);
    for _ in 0..n {
        let mut best = None;
        let mut best_d = f64::MAX;
        for (i, l) in locs.iter().enumerate() {
            if visited[i] {
                continue;
            }
            let d = dist(cx, cy, l.x, l.y);
            if d < best_d {
                best_d = d;
                best = Some(i);
            }
        }
        let i = best.unwrap();
        visited[i] = true;
        order.push(i);
        cx = locs[i].x;
        cy = locs[i].y;
    }
    order
}

fn batch_route(locs: &[PickLoc]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..locs.len()).collect();
    order.sort_by(|&a, &b| (locs[a].x, locs[a].y).cmp(&(locs[b].x, locs[b].y)));
    order
}

fn s_shape_route(locs: &[PickLoc]) -> Vec<usize> {
    let mut by_aisle: std::collections::BTreeMap<i32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, l) in locs.iter().enumerate() {
        by_aisle.entry(l.x).or_default().push(i);
    }
    let mut order = Vec::with_capacity(locs.len());
    for (aisle_idx, (_aisle, mut members)) in by_aisle.into_iter().enumerate() {
        members.sort_by_key(|&i| locs[i].y);
        if aisle_idx % 2 == 1 {
            members.reverse();
        }
        order.extend(members);
    }
    order
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RouteComparison {
    naive: f64,
    nearest_neighbor: f64,
    batch: f64,
    s_shape: f64,
    best: &'static str,
}

fn compare_routes(locs: &[PickLoc]) -> RouteComparison {
    let naive = route_distance(locs, &naive_route(locs.len()));
    let nn = route_distance(locs, &nearest_neighbor_route(locs));
    let batch = route_distance(locs, &batch_route(locs));
    let s = route_distance(locs, &s_shape_route(locs));
    let best = [
        ("naive", naive),
        ("nearest_neighbor", nn),
        ("batch", batch),
        ("s_shape", s),
    ]
    .into_iter()
    .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
    .map(|(name, _)| name)
    .unwrap();
    RouteComparison {
        naive,
        nearest_neighbor: nn,
        batch,
        s_shape: s,
        best,
    }
}

fn fmt(d: f64) -> String {
    format!("{d:.1}")
}

// ----------------------------------------------------------------------------
// Components
// ----------------------------------------------------------------------------

/// Inventory grid: the operator sees live on-hand quantities and can record
/// receives / picks / manual adjustments against a bin.
#[component]
fn InventoryPanel(rows: RwSignal<Vec<RwSignal<StockRow>>>) -> impl IntoView {
    let seed = move |_| {
        let mut rng = RngSim::new(0x1234);
        let mut v = Vec::new();
        for _ in 0..16u32 {
            let row = StockRow {
                bin: Id::new(),
                sku: Id::new(),
                qty: (rng.next() % 80) as i64 + 5,
            };
            v.push(RwSignal::new(row));
        }
        rows.set(v);
    };

    let add_received = move |s: RwSignal<StockRow>| {
        s.update(|r| r.qty += 10);
    };
    let add_pick = move |s: RwSignal<StockRow>| {
        s.update(|r| r.qty = (r.qty - 1).max(0));
    };
    let add_adjust = move |s: RwSignal<StockRow>| {
        s.update(|r| r.qty = (r.qty - 3).max(0));
    };

    view! {
        <section>
            <div class="row">
                <h2>"On-hand inventory"</h2>
                <button on:click=seed>"Load demo bins"</button>
            </div>
            <p class="muted">"Record goods-in, picks, and manual adjustments. Per-bin rows never lock the warehouse."</p>
            <table>
                <thead>
                    <tr><th>"Bin"</th><th>"SKU"</th><th>"On hand"</th><th>"Actions"</th></tr>
                </thead>
                <tbody>
                    <For
                        each=move || rows.get()
                        key=|s| s.get().bin
                        let:item
                    >
                        <tr>
                            <td><span class="pill">{move || item.get().bin.as_str()}</span></td>
                            <td><span class="pill">{move || item.get().sku.as_str()}</span></td>
                            <td class:warn=move || item.get().qty < 15>{move || item.get().qty}</td>
                            <td class="row">
                                <button on:click=move |_| add_received(item)>"+ Receive"</button>
                                <button on:click=move |_| add_pick(item)>"- Pick"</button>
                                <button on:click=move |_| add_adjust(item)>"Adjust"</button>
                            </td>
                        </tr>
                    </For>
                </tbody>
            </table>
        </section>
    }
}

/// Picker route planner: enter pick locations and compare path strategies.
#[component]
fn RoutePanel(
    locs: RwSignal<Vec<PickLoc>>,
    comparison: Signal<Option<RouteComparison>>,
) -> impl IntoView {
    let add_loc = move |_| {
        let n = locs.get().len();
        locs.update(|v| {
            v.push(PickLoc {
                id: Id::new(),
                x: (n % 6) as i32,
                y: (n * 7) as i32,
            });
        });
    };
    let seed_locs = move |_| {
        let mut rng = RngSim::new(99);
        let mut v = Vec::new();
        for _ in 0..8 {
            v.push(PickLoc {
                id: Id::new(),
                x: (rng.next() % 6) as i32,
                y: (rng.next() % 12) as i32,
            });
        }
        locs.set(v);
    };
    let clear = move |_| locs.set(Vec::new());

    view! {
        <section>
            <div class="row">
                <h2>"Picker route planner"</h2>
                <button on:click=add_loc>"Add location"</button>
                <button on:click=seed_locs>"Demo pick list"</button>
                <button on:click=clear>"Clear"</button>
            </div>
            <p class="muted">"Strategies start and end at the depot (0,0). The shortest wins the wave."</p>
            <Show
                when=move || comparison.get().is_some()
                fallback=|| view! { <p class="muted">"Add at least one location to plan a route."</p> }
            >
                {move || {
                    let c = comparison.get().unwrap();
                    let best = c.best;
                    view! {
                        <table>
                            <thead><tr><th>"Strategy"</th><th>"Travel (units)"</th></tr></thead>
                            <tbody>
                                <RouteRow name="Naive" dist=c.naive best=best />
                                <RouteRow name="Nearest neighbor" dist=c.nearest_neighbor best=best />
                                <RouteRow name="Batch / zone" dist=c.batch best=best />
                                <RouteRow name="S-shaped" dist=c.s_shape best=best />
                            </tbody>
                        </table>
                    }
                }}
            </Show>
        </section>
    }
}

#[component]
fn RouteRow(name: &'static str, dist: f64, best: &'static str) -> impl IntoView {
    let is_best = name_to_key(name) == best;
    view! {
        <tr>
            <td class:good=is_best>{name}</td>
            <td class:good=is_best>{fmt(dist)}</td>
        </tr>
    }
}

fn name_to_key(name: &str) -> &'static str {
    match name {
        "Naive" => "naive",
        "Nearest neighbor" => "nearest_neighbor",
        "Batch / zone" => "batch",
        "S-shaped" => "s_shape",
        _ => "",
    }
}

/// Tiny deterministic PRNG so demo data is reproducible (no `rand` dependency).
struct RngSim(u64);
impl RngSim {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Root component wiring together the operator panels.
#[component]
pub fn App() -> impl IntoView {
    let inventory = RwSignal::new(Vec::new());
    let locs = RwSignal::new(Vec::new());
    let comparison = Signal::derive(move || {
        let v = locs.get();
        if v.is_empty() {
            None
        } else {
            Some(compare_routes(&v))
        }
    });

    view! {
        <main>
            <h1>"TPT ERP - WMS operator view"</h1>
            <p class="muted">"3PL / warehouse management. Type-safe bins & SKUs via tpt-erp-primitives."</p>
            <InventoryPanel rows=inventory />
            <RoutePanel locs=locs comparison=comparison />
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
