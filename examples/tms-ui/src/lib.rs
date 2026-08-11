//! Leptos dispatcher live-map / route-plan view for the TMS reference ERP.
//!
//! A **front-end** mirror of the dispatcher console. It re-uses the strong-ID types from
//! [`tpt_erp_primitives`] so the same `Id` protections that guard the route engine on the
//! server also guard the dispatcher's screen.
//!
//! [`RouteStage`] is a faithful re-implementation of the route lifecycle (seed tour →
//! 2-opt refinement → dispatched), used to render live **status badges**. A vehicle grid
//! shows position (lat/lng) and speed, mirroring the GPS telemetry the server ingests.

use leptos::prelude::*;
use tpt_erp_primitives::{Entity, Id};

/// Marker entity for a vehicle (mirrors `tms` typing).
#[derive(Debug)]
pub struct Vehicle;
impl Entity for Vehicle {}

/// A live vehicle row in the dispatcher grid.
#[derive(Debug, Clone, Copy, PartialEq)]
struct VehicleRow {
    id: Id<Vehicle>,
    name: &'static str,
    lat: f64,
    lng: f64,
    speed_kmh: f64,
}

/// The lifecycle of a route plan, mirroring the server's optimization pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RouteStage {
    Seeded,
    Optimizing,
    Dispatched,
}

impl RouteStage {
    fn pipeline() -> &'static [RouteStage] {
        &[
            RouteStage::Seeded,
            RouteStage::Optimizing,
            RouteStage::Dispatched,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            RouteStage::Seeded => "Seeded",
            RouteStage::Optimizing => "Optimizing",
            RouteStage::Dispatched => "Dispatched",
        }
    }
}

#[component]
fn StageBadge(stage: RouteStage, current: Signal<RouteStage>) -> impl IntoView {
    let cls = move || {
        if stage == current.get() {
            "badge active"
        } else {
            "badge"
        }
    };
    view! { <span class=cls>{stage.label()}</span> }
}

/// The dispatcher console: a vehicle live-grid plus a route-plan badge trail.
#[component]
pub fn App() -> impl IntoView {
    let vehicles = RwSignal::new(vec![
        RwSignal::new(VehicleRow {
            id: Id::new(),
            name: "Truck 1",
            lat: 40.71,
            lng: -74.01,
            speed_kmh: 62.0,
        }),
        RwSignal::new(VehicleRow {
            id: Id::new(),
            name: "Truck 2",
            lat: 40.73,
            lng: -73.99,
            speed_kmh: 48.0,
        }),
        RwSignal::new(VehicleRow {
            id: Id::new(),
            name: "Van 7",
            lat: 40.69,
            lng: -74.02,
            speed_kmh: 0.0,
        }),
    ]);

    let current = RwSignal::new(RouteStage::Seeded);
    let advance = move |_| {
        let next = match current.get() {
            RouteStage::Seeded => RouteStage::Optimizing,
            RouteStage::Optimizing => RouteStage::Dispatched,
            RouteStage::Dispatched => RouteStage::Dispatched,
        };
        current.set(next);
    };

    let badges = RwSignal::new(RouteStage::pipeline().to_vec());

    view! {
        <main>
            <h1>"TPT ERP — Dispatcher Console"</h1>
            <p class="muted">"Fleet / TMS reference UI. Type-safe vehicle ids via tpt-erp-primitives."</p>

            <section>
                <h2>"Live Fleet"</h2>
                <table>
                    <thead>
                        <tr><th>"Vehicle"</th><th>"Lat"</th><th>"Lng"</th><th>"Speed"</th><th>"VIN"</th></tr>
                    </thead>
                    <tbody>
                        <For
                            each=move || vehicles.get()
                            key=|v| v.get().id
                            let:item
                        >
                            <tr>
                                <td>{move || item.get().name}</td>
                                <td>{move || format!("{:.3}", item.get().lat)}</td>
                                <td>{move || format!("{:.3}", item.get().lng)}</td>
                                <td class:warn=move || item.get().speed_kmh == 0.0>
                                    {move || format!("{:.0} km/h", item.get().speed_kmh)}
                                </td>
                                <td><span class="pill">{move || item.get().id.as_str()}</span></td>
                            </tr>
                        </For>
                    </tbody>
                </table>
            </section>

            <section>
                <h2>"Route Plan"</h2>
                <div class="badges">
                    <For
                        each=move || badges.get()
                        key=|s| *s
                        let:item
                    >
                        <StageBadge stage=item current=current.into() />
                    </For>
                </div>
                <button on:click=advance>"Advance plan →"</button>
            </section>
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
