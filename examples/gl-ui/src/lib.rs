//! Leptos operator UI for the GL reference ERP.
//!
//! This is the **front-end** mirror of the accounting console. It reuses the exact same
//! [`gl`] journal engine that powers the server: the demo tenant's books are posted with
//! the real event-sourced, double-entry [`gl::journal::JournalEngine`], and the trial
//! balance / balance sheet are built by the same CQRS reporting read models — proving the
//! UI and the API share one source of truth.

use std::sync::{Arc, Mutex};

use leptos::prelude::*;
use rust_decimal::Decimal;
use tpt_erp_ledger::{AccountId, EntrySide, LedgerEntry};
use tpt_erp_primitives::{Money, Usd};

use gl::coa::DemoAccounts;
use gl::demo_tenant;
use gl::journal::JournalEngine;
use gl::reporting::{balance_sheet_now, trial_balance_now};

/// One journal leg in the USD demo ledger.
fn leg(account: AccountId, side: EntrySide, amount: i64) -> LedgerEntry<Usd> {
    LedgerEntry {
        account,
        side,
        amount: Money::<Usd>::new(Decimal::from(amount)),
    }
}

/// Shared UI state: the live journal engine plus the well-known demo account ids.
type GlState = (JournalEngine<Usd>, DemoAccounts<Usd>);
/// `RwSignal` requires `Clone`; wrap the non-`Clone` engine in `Arc<Mutex<…>>`.
type UiState = Arc<Mutex<GlState>>;

/// A journal-entry panel: post sample transactions and close the period. Every post goes
/// through the real [`JournalEngine::post_transaction_sync`], so the books can never
/// become unbalanced (the double-entry check rejects bad entries before they post).
#[component]
fn JournalPanel(state: RwSignal<UiState>) -> impl IntoView {
    let post_sale = move |_| {
        state.update(|arc| {
            let g = arc.lock().unwrap();
            let _ = g.0.post_transaction_sync(
                vec![
                    leg(g.1.cash, EntrySide::Debit, 100),
                    leg(g.1.sales_revenue, EntrySide::Credit, 100),
                ],
                "2026-01",
                "sale",
            );
        });
    };
    let post_expense = move |_| {
        state.update(|arc| {
            let g = arc.lock().unwrap();
            let _ = g.0.post_transaction_sync(
                vec![
                    leg(g.1.cogs, EntrySide::Debit, 40),
                    leg(g.1.cash, EntrySide::Credit, 40),
                ],
                "2026-01",
                "cogs",
            );
        });
    };
    let close_books = move |_| {
        state.update(|arc| {
            let g = arc.lock().unwrap();
            let entries = g.0.generate_closing_entries(g.1.retained_earnings);
            let _ =
                g.0.post_transaction_sync(entries, "2026-01", "period close");
        });
    };

    view! {
        <section>
            <h2>"Journal entry"</h2>
            <p class="muted">
                "Each button posts a real balanced transaction through the event-sourced engine."
            </p>
            <div class="row">
                <button on:click=post_sale>"Post sale (100)"</button>
                <button on:click=post_expense>"Post expense (40)"</button>
                <button on:click=close_books>"Close books"</button>
            </div>
        </section>
    }
}

/// A live trial balance derived from the engine's read model.
#[component]
fn TrialBalancePanel(state: RwSignal<UiState>) -> impl IntoView {
    let tb = Signal::derive(move || {
        let s = state.get();
        let (eng, _) = &*s.lock().unwrap();
        trial_balance_now(eng)
    });

    view! {
        <section>
            <h2>"Trial balance"</h2>
            <table>
                <thead>
                    <tr>
                        <th>"Code"</th>
                        <th>"Account"</th>
                        <th>"Debits"</th>
                        <th>"Credits"</th>
                        <th>"Balance"</th>
                    </tr>
                </thead>
                <tbody>
                    {move || {
                        tb.get()
                            .rows
                            .into_iter()
                            .map(|r| {
                                view! {
                                    <tr>
                                        <td>{r.code}</td>
                                        <td>{r.name}</td>
                                        <td>{r.account_balance.debits.to_string()}</td>
                                        <td>{r.account_balance.credits.to_string()}</td>
                                        <td class:good=(r.signed_balance.amount() >= Decimal::ZERO)>
                                            {r.signed_balance.to_string()}
                                        </td>
                                    </tr>
                                }
                            })
                            .collect_view()
                    }}
                </tbody>
                <tfoot>
                    <tr>
                        <td></td>
                        <td><strong>"Totals"</strong></td>
                        <td><strong>{move || tb.get().total_debits.to_string()}</strong></td>
                        <td><strong>{move || tb.get().total_credits.to_string()}</strong></td>
                        <td></td>
                    </tr>
                </tfoot>
            </table>
            <p class:good=(move || tb.get().is_balanced()) class:warn=(move || !tb.get().is_balanced())>
                {move || {
                    if tb.get().is_balanced() {
                        "Books balance".to_string()
                    } else {
                        "OUT OF BALANCE".to_string()
                    }
                }}
            </p>
        </section>
    }
}

/// A live balance sheet (assets = liabilities + equity, incl. net income).
#[component]
fn BalanceSheetPanel(state: RwSignal<UiState>) -> impl IntoView {
    let bs = Signal::derive(move || {
        let s = state.get();
        let (eng, _) = &*s.lock().unwrap();
        balance_sheet_now(eng)
    });

    view! {
        <section>
            <h2>"Balance sheet"</h2>
            <table>
                <thead>
                    <tr><th>"Section"</th><th>"Account"</th><th>"Balance"</th></tr>
                </thead>
                <tbody>
                    <tr><td colspan="3"><strong>"Assets"</strong></td></tr>
                    {move || {
                        bs.get()
                            .assets
                            .into_iter()
                            .map(|r| {
                                view! {
                                    <tr>
                                        <td></td>
                                        <td>{r.name}</td>
                                        <td>{r.balance.to_string()}</td>
                                    </tr>
                                }
                            })
                            .collect_view()
                    }}
                    <tr><td colspan="3"><strong>"Liabilities"</strong></td></tr>
                    {move || {
                        bs.get()
                            .liabilities
                            .into_iter()
                            .map(|r| {
                                view! {
                                    <tr>
                                        <td></td>
                                        <td>{r.name}</td>
                                        <td>{r.balance.to_string()}</td>
                                    </tr>
                                }
                            })
                            .collect_view()
                    }}
                    <tr><td colspan="3"><strong>"Equity"</strong></td></tr>
                    {move || {
                        bs.get()
                            .equity
                            .into_iter()
                            .map(|r| {
                                view! {
                                    <tr>
                                        <td></td>
                                        <td>{r.name}</td>
                                        <td>{r.balance.to_string()}</td>
                                    </tr>
                                }
                            })
                            .collect_view()
                    }}
                    <tr>
                        <td></td>
                        <td><strong>"Net income"</strong></td>
                        <td><strong>{move || bs.get().net_income.to_string()}</strong></td>
                    </tr>
                </tbody>
            </table>
            <p class:good=(move || bs.get().is_balanced())>
                {move || {
                    if bs.get().is_balanced() {
                        format!(
                            "Balanced: A {} = L {} + E {}",
                            bs.get().total_assets,
                            bs.get().total_liabilities,
                            bs.get().total_equity,
                        )
                    } else {
                        "OUT OF BALANCE".to_string()
                    }
                }}
            </p>
        </section>
    }
}

/// Root component: seeds a demo ledger, then wires the journal-entry and reporting views.
#[component]
pub fn App() -> impl IntoView {
    let (eng, d) = gl::journal::demo(demo_tenant());
    // Seed a couple of opening transactions so the views are non-empty on first render.
    let _ = eng.post_transaction_sync(
        vec![
            leg(d.cash, EntrySide::Debit, 100),
            leg(d.sales_revenue, EntrySide::Credit, 100),
        ],
        "2026-01",
        "opening sale",
    );
    let _ = eng.post_transaction_sync(
        vec![
            leg(d.cogs, EntrySide::Debit, 40),
            leg(d.cash, EntrySide::Credit, 40),
        ],
        "2026-01",
        "opening cogs",
    );
    let state = RwSignal::new(Arc::new(Mutex::new((eng, d))));

    view! {
        <main>
            <h1>"TPT ERP - GL operator view"</h1>
            <p class="muted">
                "Accounting / General Ledger. Double-entry postings via tpt-erp-ledger; the trial \
                 balance and balance sheet are CQRS read models."
            </p>
            <JournalPanel state=state />
            <TrialBalancePanel state=state />
            <BalanceSheetPanel state=state />
        </main>
    }
}

// `trunk` + `wasm32` entry point. Not used when compiling for the host target.
#[cfg(target_arch = "wasm32")]
fn main() {
    leptos::mount::mount_to_body(App);
}
