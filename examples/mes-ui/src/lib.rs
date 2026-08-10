//! Leptos operator UI for the MES reference ERP.
//!
//! This is the **front-end** mirror of the shop-floor operator console. It re-uses
//! [`tpt_erp_primitives::StateMachine`] to model WIP lifecycle on the client, so the
//! exact same transition graph that guards the server's `examples/mes` engine also
//! guards the operator's buttons: an illegal jump is impossible to even click.
//!
//! QC pass/fail entries feed a live OEE dashboard whose math mirrors
//! `examples/mes/src/oee.rs`.

use leptos::prelude::*;
use tpt_erp_primitives::{Entity, Id, StateMachine};

/// Marker entity for a shop-floor work-in-process item.
#[derive(Debug)]
pub struct Wip;
impl Entity for Wip {}

/// The lifecycle of a work-in-process item on the shop floor.
///
/// Mirrors `examples/mes/src/wip.rs`: `Raw` may go to `Machined` or `Welded`;
/// both converge on `Assembled` then `Inspected`, which yields `Finished` or `Scrapped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, StateMachine)]
#[state_machine(transitions(
    Raw => Machined,
    Raw => Welded,
    Machined => Assembled,
    Welded => Assembled,
    Assembled => Inspected,
    Inspected => Finished,
    Inspected => Scrapped,
))]
pub enum WipState {
    Raw,
    Machined,
    Welded,
    Assembled,
    Inspected,
    Finished,
    Scrapped,
}

impl WipState {
    /// All states, used to enumerate legal next transitions for the UI.
    fn all() -> &'static [WipState] {
        &[
            WipState::Raw,
            WipState::Machined,
            WipState::Welded,
            WipState::Assembled,
            WipState::Inspected,
            WipState::Finished,
            WipState::Scrapped,
        ]
    }

    /// Legal next states from `self` (excludes self).
    fn next(self) -> Vec<WipState> {
        WipState::all()
            .iter()
            .copied()
            .filter(|s| *s != self && self.can_transition(*s))
            .collect()
    }
}

/// A tracked WIP item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WipItem {
    id: Id<Wip>,
    state: WipState,
}

impl WipItem {
    fn new() -> Self {
        Self {
            id: Id::new(),
            state: WipState::Raw,
        }
    }
}

// ----------------------------------------------------------------------------
// OEE math (mirrors `examples/mes/src/oee.rs`).
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct ProductionRun {
    planned_time_secs: f64,
    run_time_secs: f64,
    ideal_cycle_time_secs: f64,
    total_count: u64,
    good_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Oee {
    availability: f64,
    performance: f64,
    quality: f64,
    oee: f64,
}

fn clamp01(x: f64) -> f64 {
    if !x.is_finite() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

impl ProductionRun {
    fn oee(&self) -> Oee {
        let availability = clamp01(self.run_time_secs / self.planned_time_secs);
        let performance =
            clamp01((self.ideal_cycle_time_secs * self.total_count as f64) / self.run_time_secs);
        let quality = clamp01(self.good_count as f64 / self.total_count as f64);
        Oee {
            availability,
            performance,
            quality,
            oee: availability * performance * quality,
        }
    }
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

// ----------------------------------------------------------------------------
// Components
// ----------------------------------------------------------------------------

/// WIP board: each item shows its current state and only its legal transitions.
#[component]
fn WipPanel(items: RwSignal<Vec<RwSignal<WipItem>>>) -> impl IntoView {
    let add = move |_| items.update(|v| v.push(RwSignal::new(WipItem::new())));

    view! {
        <section>
            <div class="row">
                <h2>"Work-in-process"</h2>
                <button on:click=add>"New item"</button>
            </div>
            <p class="muted">"Only legal transitions are clickable. The StateMachine derive rejects the rest."</p>
            <table>
                <thead><tr><th>"Item"</th><th>"State"</th><th>"Advance to"</th></tr></thead>
                <tbody>
                    <For
                        each=move || items.get()
                        key=|s| s.get().id
                        let:item
                    >
                        <WipRow item=item />
                    </For>
                </tbody>
            </table>
        </section>
    }
}

/// A single WIP item row with its legal transition buttons.
/// Reads `item` reactively so the row re-renders after each transition.
#[component]
fn WipRow(item: RwSignal<WipItem>) -> impl IntoView {
    view! {
        <tr>
            <td><span class="pill">{move || item.get().id.as_str()}</span></td>
            <td class:good=move || matches!(item.get().state, WipState::Finished | WipState::Scrapped)>
                {move || format!("{:?}", item.get().state)}
            </td>
            <td class="row">
                {move || {
                    let next = item.get().state.next();
                    next
                        .into_iter()
                        .map(|to| {
                            let item2 = item;
                            view! {
                                <button
                                    on:click=move |_| {
                                        item2.update(|it| {
                                            if it.state.transition(to).is_ok() {
                                                it.state = to;
                                            }
                                        });
                                    }
                                >
                                    {format!("{to:?}")}
                                </button>
                            }
                        })
                        .collect_view()
                }}
                <span class="muted">
                    {move || {
                        if matches!(item.get().state, WipState::Finished | WipState::Scrapped) {
                            "terminal"
                        } else {
                            ""
                        }
                    }}
                </span>
            </td>
        </tr>
    }
}

/// QC entry: record pass/fail results that drive the OEE quality factor.
#[component]
fn QcPanel(good: RwSignal<u64>, total: RwSignal<u64>, last: RwSignal<String>) -> impl IntoView {
    let pass = move |_| {
        good.update(|g| *g += 1);
        total.update(|t| *t += 1);
        last.set("QC: 1 passed".to_string());
    };
    let fail = move |_| {
        total.update(|t| *t += 1);
        last.set("QC: 1 failed".to_string());
    };

    view! {
        <section>
            <h2>"QC entry"</h2>
            <div class="row">
                <button on:click=pass>"Pass"</button>
                <button on:click=fail>"Fail"</button>
                <span class="muted">{move || last.get()}</span>
            </div>
            <p class="muted">"Good / total feed the Quality factor on the OEE dashboard."</p>
        </section>
    }
}

/// Live OEE dashboard: time inputs plus QC-derived counts, computed reactively.
#[component]
fn OeeDashboard(
    planned: RwSignal<String>,
    run: RwSignal<String>,
    cycle: RwSignal<String>,
    good: RwSignal<u64>,
    total: RwSignal<u64>,
) -> impl IntoView {
    let oee = Signal::derive(move || {
        let parse = |s: &str| s.parse::<f64>().unwrap_or(0.0);
        ProductionRun {
            planned_time_secs: parse(&planned.get()).max(1.0),
            run_time_secs: parse(&run.get()),
            ideal_cycle_time_secs: parse(&cycle.get()).max(1e-6),
            total_count: total.get(),
            good_count: good.get(),
        }
        .oee()
    });

    view! {
        <section>
            <h2>"OEE dashboard"</h2>
            <div class="row">
                <label>"Planned (s)" <input type="number" prop:value=move || planned.get() on:input=move |ev| planned.set(event_target_value(&ev)) /></label>
                <label>"Run (s)" <input type="number" prop:value=move || run.get() on:input=move |ev| run.set(event_target_value(&ev)) /></label>
                <label>"Ideal cycle (s)" <input type="number" prop:value=move || cycle.get() on:input=move |ev| cycle.set(event_target_value(&ev)) /></label>
            </div>
            <p class="muted">"OEE = Availability x Performance x Quality. World-class ~ 85%."</p>
            {move || {
                let o = oee.get();
                view! {
                    <Bar label="Availability" value=o.availability />
                    <Bar label="Performance" value=o.performance />
                    <Bar label="Quality" value=o.quality />
                    <p>"Overall OEE: " <strong class:good=(o.oee >= 0.85) class:warn=(o.oee < 0.6)>{pct(o.oee)}</strong></p>
                }
            }}
        </section>
    }
}

#[component]
fn Bar(label: &'static str, value: f64) -> impl IntoView {
    let p = (value.clamp(0.0, 1.0) * 100.0) as u32;
    view! {
        <div class="row" style="margin:.3rem 0">
            <span style="width:7rem">{label}</span>
            <div class="bar" style="flex:1"><span style=format!("width:{}%", p)></span></div>
            <span class="muted">{pct(value)}</span>
        </div>
    }
}

/// Root component wiring the operator console together.
#[component]
pub fn App() -> impl IntoView {
    let items = RwSignal::new(Vec::new());
    let good = RwSignal::new(40u64);
    let total = RwSignal::new(44u64);
    let last = RwSignal::new(String::new());
    let planned = RwSignal::new("480".to_string());
    let run = RwSignal::new("432".to_string());
    let cycle = RwSignal::new("1".to_string());

    view! {
        <main>
            <h1>"TPT ERP - MES operator view"</h1>
            <p class="muted">"Manufacturing execution. WIP lifecycle guarded by tpt-erp-primitives StateMachine."</p>
            <WipPanel items=items />
            <QcPanel good=good total=total last=last />
            <OeeDashboard planned=planned run=run cycle=cycle good=good total=total />
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
