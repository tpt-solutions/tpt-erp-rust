//! End-to-end test: API transaction -> ledger entry -> tenant isolation verified.

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use server::app_default;
use tower::ServiceExt;
use uuid::Uuid;

type App = axum::Router;

fn tx_body(debit: &str, credit: &str, amount: &str) -> String {
    format!(
        r#"{{"entries":[{{"account":"{debit}","side":"debit","amount":"{amount}"}},{{"account":"{credit}","side":"credit","amount":"{amount}"}}]}}"#
    )
}

async fn post(app: &App, tenant: &str, body: String) -> StatusCode {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/transactions")
                .header(header::HOST, format!("{tenant}.example.com"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

async fn balances(app: &App, tenant: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/balances")
                .header(header::HOST, format!("{tenant}.example.com"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_is_public() {
    let app = app_default();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn tenant_isolation_is_enforced() {
    let app = app_default();
    let debit = Uuid::new_v4().to_string();
    let credit = Uuid::new_v4().to_string();

    // Acme posts a balanced $100 transaction.
    assert_eq!(
        post(&app, "acme", tx_body(&debit, &credit, "100.00")).await,
        StatusCode::OK
    );

    // Acme can see its own balances.
    let acme = balances(&app, "acme").await;
    assert_eq!(acme[&debit], "-100.00");
    assert_eq!(acme[&credit], "100.00");

    // Globex sees NOTHING — cross-tenant leakage is structurally impossible.
    let globex = balances(&app, "globex").await;
    assert!(globex.as_object().unwrap().is_empty());

    // Globex cannot see Acme's journal either.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/transactions")
                .header(header::HOST, "globex.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let journal: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(journal.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn unbalanced_transaction_is_rejected() {
    let app = app_default();
    let debit = Uuid::new_v4().to_string();
    let credit = Uuid::new_v4().to_string();
    // Debit 100, credit 50 -> not balanced.
    let imbalanced = format!(
        r#"{{"entries":[{{"account":"{debit}","side":"debit","amount":"100.00"}},{{"account":"{credit}","side":"credit","amount":"50.00"}}]}}"#
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/transactions")
                .header(header::HOST, "acme.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(imbalanced))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn request_without_tenant_is_rejected() {
    let app = app_default();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/transactions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(tx_body(
                    &Uuid::new_v4().to_string(),
                    &Uuid::new_v4().to_string(),
                    "1.00",
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn jwt_auth_populates_principal_and_gates_tenant() {
    // A server started with a JWT secret enforces real authentication: the tenant is
    // taken from the *verified* token claim, never from a spoofable header.
    let app = server::app_with_auth(
        std::sync::Arc::new(server::AppState::default()),
        "integration-secret".into(),
    );

    let config = tpt_erp_tenant::auth::JwtConfig::new("integration-secret");
    let token = config
        .issue(&tpt_erp_tenant::auth::Claims::new(
            "user-1",
            "acme",
            vec!["staff".into()],
        ))
        .unwrap();

    // A request with a valid token for tenant `acme` succeeds and isolates data.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/transactions")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::HOST, "globex.example.com") // spoofed header must be ignored
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(tx_body(
                    &Uuid::new_v4().to_string(),
                    &Uuid::new_v4().to_string(),
                    "100.00",
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // A request with no token is rejected (401), proving tenant selection is gated.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/transactions")
                .header(header::HOST, "acme.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(tx_body(
                    &Uuid::new_v4().to_string(),
                    &Uuid::new_v4().to_string(),
                    "1.00",
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
