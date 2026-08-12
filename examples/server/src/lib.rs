//! Reference Axum server: a type-safe, multi-tenant ledger API.
//!
//! Wires [`tpt_erp_tenant`] (tenant identification + `SET LOCAL` context) with
//! [`tpt_erp_ledger`] (append-only event store + double-entry core). Each tenant gets its
//! own event store, so isolation is structural: a request can only ever read or write its
//! own tenant's journal.
//!
//! Routes (all require a resolved tenant via `X-Tenant-Id` header or `Host` subdomain):
//! - `POST /transactions` — post a balanced double-entry transaction.
//! - `GET  /balances`     — account balances for the caller's tenant.
//! - `GET  /transactions` — the caller's tenant journal.
//! - `GET  /health`       — liveness probe.

use axum::extract::{Extension, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use tpt_erp_ledger::{
    BalanceProjection, Event, EventStore, InMemoryEventStore,
    double_entry::{
        AccountId, DoubleEntry, DoubleEntryError, EntrySide, LedgerEntry, LedgerEvent, Transaction,
    },
    replay,
};
use tpt_erp_primitives::{Entity, Id, Money, Usd};
use tpt_erp_tenant::TenantContext;

/// Marker for a per-tenant ledger journal aggregate.
#[derive(Debug)]
pub struct Journal;
impl Entity for Journal {}

/// Strong id for a tenant's journal.
pub type JournalId = Id<Journal>;

/// In-memory, tenant-scoped ledger. A production deployment would back each tenant's
/// journal with a Postgres event store; the API surface is identical.
#[derive(Default)]
pub struct AppState {
    ledgers: Mutex<HashMap<TenantIdKey, TenantLedger>>,
}

type TenantIdKey = tpt_erp_tenant::TenantId;

struct TenantLedger {
    journal: JournalId,
    store: InMemoryEventStore<JournalId>,
}

impl AppState {
    /// Append a posted transaction to the tenant's journal.
    fn post(
        &self,
        tenant: &TenantIdKey,
        tx: &Transaction<Usd>,
    ) -> Result<u64, tpt_erp_ledger::EventStoreError> {
        let mut ledgers = self.ledgers.lock();
        let entry = ledgers.entry(*tenant).or_insert_with(|| TenantLedger {
            journal: JournalId::new(),
            store: InMemoryEventStore::default(),
        });
        let event = Event::new(
            entry.journal,
            "TransactionPosted",
            &LedgerEvent::TransactionPosted(tx.clone()),
        )?;
        Ok(entry.store.append(event).sequence)
    }

    /// Rebuild the tenant's balances from its journal.
    async fn balances(&self, tenant: &TenantIdKey) -> HashMap<String, String> {
        // Collect events while holding the lock; drop the guard before `.await` so the
        // future stays `Send` (required by axum handlers).
        let events: Vec<LedgerEvent<Usd>> = {
            let ledgers = self.ledgers.lock();
            let Some(entry) = ledgers.get(tenant) else {
                return HashMap::new();
            };
            entry
                .store
                .log()
                .iter()
                .filter(|e| e.aggregate_id == entry.journal)
                .filter_map(|e| LedgerEvent::<Usd>::from_payload(&e.payload).ok())
                .collect()
        };
        let proj = replay(BalanceProjection::<Usd>::default(), events)
            .await
            .expect("projection cannot fail");
        proj.balances
            .into_iter()
            .map(|(acc, money)| (acc.to_string(), money.amount().to_string()))
            .collect()
    }

    /// The raw journal payloads for a tenant.
    fn journal(&self, tenant: &TenantIdKey) -> Vec<Value> {
        let ledgers = self.ledgers.lock();
        let Some(entry) = ledgers.get(tenant) else {
            return Vec::new();
        };
        entry
            .store
            .log()
            .iter()
            .filter(|e| e.aggregate_id == entry.journal)
            .map(|e| e.payload.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Request / response DTOs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PostTransaction {
    pub entries: Vec<EntryDto>,
}

#[derive(Deserialize)]
pub struct EntryDto {
    pub account: String,
    pub side: String,
    pub amount: String,
}

/// Errors surfaced by the transaction endpoint.
#[derive(Debug, thiserror::Error)]
pub enum PostError {
    #[error("invalid entry: {0}")]
    BadEntry(String),
    #[error("transaction is not balanced: {0}")]
    Unbalanced(String),
    #[error("invalid JSON: {0}")]
    Json(#[from] axum::extract::rejection::JsonRejection),
}

impl IntoResponse for PostError {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match &self {
            PostError::BadEntry(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            PostError::Unbalanced(_) => (StatusCode::UNPROCESSABLE_ENTITY, self.to_string()),
            PostError::Json(_) => (StatusCode::BAD_REQUEST, self.to_string()),
        };
        (status, msg).into_response()
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> &'static str {
    "ok"
}

async fn post_transaction(
    Extension(state): Extension<Arc<AppState>>,
    TenantContext { id, .. }: TenantContext,
    Json(body): Json<PostTransaction>,
) -> Result<Json<Value>, PostError> {
    let mut entries = Vec::with_capacity(body.entries.len());
    for (i, e) in body.entries.iter().enumerate() {
        let account = AccountId::parse(&e.account)
            .map_err(|err| PostError::BadEntry(format!("entry {i}: bad account: {err}")))?;
        let side = match e.side.to_ascii_lowercase().as_str() {
            "debit" => EntrySide::Debit,
            "credit" => EntrySide::Credit,
            other => {
                return Err(PostError::BadEntry(format!(
                    "entry {i}: unknown side '{other}'"
                )));
            }
        };
        let amount = Decimal::from_str_exact(&e.amount)
            .map_err(|err| PostError::BadEntry(format!("entry {i}: bad amount: {err}")))?;
        entries.push(LedgerEntry {
            account,
            side,
            amount: Money::new(amount),
        });
    }

    let tx = Transaction {
        id: tpt_erp_ledger::double_entry::TransactionId::new(),
        entries,
    };

    tx.validate().map_err(|err| match err {
        DoubleEntryError::Unbalanced {
            debits, credits, ..
        } => PostError::Unbalanced(format!("debits {debits} != credits {credits}")),
        DoubleEntryError::Empty { .. } => PostError::BadEntry("need at least two entries".into()),
    })?;

    let seq = state
        .post(&id, &tx)
        .expect("event store append is infallible here");
    Ok(Json(
        serde_json::json!({ "sequence": seq, "id": tx.id.to_string() }),
    ))
}

async fn balances(
    Extension(state): Extension<Arc<AppState>>,
    TenantContext { id, .. }: TenantContext,
) -> Json<Value> {
    let map = state.balances(&id).await;
    Json(serde_json::to_value(&map).expect("balances are serializable"))
}

async fn journal(
    Extension(state): Extension<Arc<AppState>>,
    TenantContext { id, .. }: TenantContext,
) -> Json<Vec<Value>> {
    Json(state.journal(&id))
}

/// Build the application router.
///
/// `AppState` is injected via an [`Extension`] layer (so the served router stays
/// `Router<()>` and works with `axum::serve`), while the tenant context is resolved by
/// the [`tpt_erp_tenant`] middleware.
pub fn app(state: Arc<AppState>) -> Router {
    let shared = state.clone();
    Router::new()
        .route("/health", get(health))
        .route("/transactions", post(post_transaction).get(journal))
        .route("/balances", get(balances))
        .layer(axum::middleware::from_fn(
            move |mut req: Request, next: Next| {
                metrics::counter!("tpt_http_requests_total").increment(1);
                req.extensions_mut().insert(shared.clone());
                next.run(req)
            },
        ))
        .layer(axum::middleware::from_fn(
            tpt_erp_tenant::web::tenant_context_middleware,
        ))
}

/// Build the default app (fresh in-memory state).
pub fn app_default() -> Router {
    app(Arc::new(AppState::default()))
}
