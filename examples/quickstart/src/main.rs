//! # 10-minute quickstart — a type-safe, multi-tenant CRUD API.
//!
//! This single file proves the Phase 1 milestone: with `tpt-primitives`,
//! `TptEntity`, and `TptApi` you can stand up a fully type-safe CRUD API —
//! with currency/money safety, strong IDs, transition-checked workflows,
//! validation, audit fields, pagination, and RBAC — in well under 10 minutes.
//!
//! Run it (`cargo run -p quickstart`) to watch the API serve a few requests
//! in-process via `tower::oneshot`.

use std::sync::Arc;

use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use serde_json::json;
use tower::ServiceExt;
use tpt_entity::{
    AllowAll, Auditable, EntityTable, InMemoryRepository, TptApi, TptEntity, Validatable,
};
use tpt_primitives::{Entity, Id, Money, StateMachine, Usd};

impl Entity for Customer {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, StateMachine)]
#[state_machine(transitions(
    Draft => Confirmed,
    Confirmed => Shipped,
))]
enum OrderStatus {
    Draft,
    Confirmed,
    Shipped,
}

/// A tenant-scoped customer. The `#[id]` field becomes the primary key, the
/// `#[validate(...)]` attributes become the insert/update guard, and the
/// `#[audit]` fields feed the `Auditable` trait.
#[derive(Clone, serde::Serialize, serde::Deserialize, TptEntity)]
#[tpt_entity(table = "customers")]
struct Customer {
    #[id]
    id: Id<Customer>,
    #[validate(required, len(min = 1, max = 200), email)]
    email: String,
    #[validate(range(min = 0, max = 120))]
    age: i32,
    #[audit]
    created_at: DateTime<Utc>,
    #[audit]
    updated_at: DateTime<Utc>,
    #[audit]
    created_by: Option<String>,
    #[audit]
    updated_by: Option<String>,
}

/// The Axum router for `Customer` is generated from this marker struct.
#[derive(TptApi)]
#[tpt_api(entity = Customer, path = "/customers")]
struct CustomerApi;

#[tokio::main]
async fn main() {
    // --- 1. Founding primitives are unchanged ----------------------------
    let price = Money::<Usd>::from_major(19);
    let _line = price * rust_decimal::Decimal::from(3); // cross-currency math is a compile error

    // --- 2. Build the CRUD API (no hand-written routes) -----------------
    let repo = Arc::new(InMemoryRepository::<Customer>::new());
    let app = CustomerApi::router::<_, AllowAll>(repo);

    let now = Utc::now();
    let id = Id::<Customer>::new();

    // --- 3. POST /customers ----------------------------------------------
    let create_body = json!({
        "id": id.to_string(),
        "email": "ada@example.com",
        "age": 36,
        "created_at": now,
        "updated_at": now,
        "created_by": "seed",
        "updated_by": "seed",
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/customers")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    println!("POST /customers -> {}", resp.status());

    // --- 4. GET /customers (paginated + filtered) -----------------------
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/customers?page=1&per_page=10")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    println!("GET /customers status={status} body={raw}");
    let page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    println!("GET /customers -> {} row(s)", page["total"]);

    // --- 5. GET /customers/:id -------------------------------------------
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/customers/{}", id))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    println!("GET /customers/{} -> {}", id, resp.status());

    // --- 6. Validation rejects bad input ---------------------------------
    let bad = json!({ "id": Id::<Customer>::new().to_string(), "email": "not-an-email", "age": 9, "created_at": now, "updated_at": now, "created_by": "x", "updated_by": "x" });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/customers")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(bad.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    println!("POST /customers (invalid) -> {}", resp.status());

    // --- 7. Runtime traits work directly --------------------------------
    let c = Customer {
        id,
        email: "ada@example.com".into(),
        age: 36,
        created_at: now,
        updated_at: now,
        created_by: Some("seed".into()),
        updated_by: Some("seed".into()),
    };
    assert!(c.validate().is_ok());
    println!(
        "table={} id={} audit_created_by={:?}",
        Customer::table_name(),
        c.id(),
        c.audit_fields().map(|a| a.created_by)
    );

    // --- 8. State machine still guards workflows ------------------------
    let mut status = OrderStatus::Draft;
    status = status
        .transition(OrderStatus::Confirmed)
        .expect("Draft -> Confirmed");
    status = status
        .transition(OrderStatus::Shipped)
        .expect("Confirmed -> Shipped");
    println!("order status advanced to {status:?}");
    let _ = &_line;
}
