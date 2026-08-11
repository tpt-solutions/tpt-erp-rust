//! End-to-end test of the `TptEntity` + `TptApi` derives using the in-memory
//! repository and `tower::oneshot` against the generated Axum router.

use std::sync::Arc;

use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;
use tpt_erp_entity::{
    AllowAll, Auditable, AuthError, AuthPolicy, EntityTable, InMemoryRepository, Operation,
    Principal, TptApi, TptEntity, Validatable,
};
use tpt_erp_primitives::{Entity, Id};

impl Entity for Product {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TptEntity)]
#[tpt_entity(table = "products")]
struct Product {
    #[id]
    id: Id<Product>,
    #[validate(required, len(min = 1, max = 120))]
    name: String,
    price: i64,
    #[audit]
    created_at: DateTime<Utc>,
    #[audit]
    updated_at: DateTime<Utc>,
    #[audit]
    created_by: Option<String>,
    #[audit]
    updated_by: Option<String>,
}

#[derive(TptApi)]
#[tpt_api(entity = Product, path = "/products")]
struct ProductApi;

fn sample(id: Id<Product>, name: &str, now: DateTime<Utc>) -> Value {
    json!({
        "id": id.to_string(),
        "name": name,
        "price": 100,
        "created_at": now,
        "updated_at": now,
        "created_by": "tester",
        "updated_by": "tester",
    })
}

async fn request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            body.map(|b| b.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

#[tokio::test]
async fn crud_roundtrip_and_filtering() {
    let repo = Arc::new(InMemoryRepository::<Product>::new());
    let app = ProductApi::router::<_, AllowAll>(repo);
    let now = Utc::now();

    let id1 = Id::<Product>::new();
    let id2 = Id::<Product>::new();

    // create two products
    let r = request(&app, "POST", "/products", Some(sample(id1, "Widget", now))).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = request(&app, "POST", "/products", Some(sample(id2, "Gadget", now))).await;
    assert_eq!(r.status(), StatusCode::CREATED);

    // duplicate id -> conflict
    let r = request(&app, "POST", "/products", Some(sample(id1, "Widget", now))).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);

    // list all
    let r = request(&app, "GET", "/products?page=1&per_page=10", None).await;
    assert_eq!(r.status(), StatusCode::OK);
    let page: Value =
        serde_json::from_slice(&to_bytes(r.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(page["total"], 2);

    // filter by name
    let r = request(&app, "GET", "/products?name=Widget", None).await;
    let page: Value =
        serde_json::from_slice(&to_bytes(r.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["name"], "Widget");

    // get one
    let r = request(&app, "GET", &format!("/products/{}", id1), None).await;
    assert_eq!(r.status(), StatusCode::OK);

    // missing -> 404
    let r = request(
        &app,
        "GET",
        &format!("/products/{}", Id::<Product>::new()),
        None,
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    // delete
    let r = request(&app, "DELETE", &format!("/products/{}", id1), None).await;
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let r = request(&app, "GET", &format!("/products/{}", id1), None).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validation_rejects_bad_input() {
    let repo = Arc::new(InMemoryRepository::<Product>::new());
    let app = ProductApi::router::<_, AllowAll>(repo);
    let now = Utc::now();

    // empty name violates `required` + `len`
    let bad = json!({
        "id": Id::<Product>::new().to_string(),
        "name": "",
        "price": 1,
        "created_at": now,
        "updated_at": now,
        "created_by": "x",
        "updated_by": "x",
    });
    let r = request(&app, "POST", "/products", Some(bad)).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn traits_and_filter_compile_and_work() {
    let now = Utc::now();
    let id = Id::<Product>::new();
    let p = Product {
        id,
        name: "Sprocket".into(),
        price: 42,
        created_at: now,
        updated_at: now,
        created_by: Some("a".into()),
        updated_by: Some("b".into()),
    };

    assert_eq!(Product::table_name(), "products");
    assert_eq!(p.id(), id);
    assert!(p.validate().is_ok());
    assert!(p.audit_fields().is_some());

    // the generated filter matches by equality
    let mut f = <Product as EntityTable>::Filter::default();
    f.name = Some("Sprocket".into());
    assert!(tpt_erp_entity::ApplyFilter::<Product>::matches(&f, &p));
    f.name = Some("Nope".into());
    assert!(!tpt_erp_entity::ApplyFilter::<Product>::matches(&f, &p));
}

/// Policy that only authorizes operations for a principal carrying the `admin` role.
/// Used to prove the generated router honours a custom `AuthPolicy` with a real
/// (non-default) `Principal` rather than silently clobbering it.
struct AdminOnly;

impl AuthPolicy for AdminOnly {
    fn authorize(op: Operation, principal: &Principal) -> Result<(), AuthError> {
        if principal.roles.iter().any(|r| r == "admin") {
            Ok(())
        } else {
            Err(AuthError(op))
        }
    }
}

/// Inject a `Principal` with the given roles into every request's extensions,
/// emulating an upstream auth middleware (JWT/JTW tenant resolution).
async fn inject_principal(roles: Vec<String>, req: axum::extract::Request, next: Next) -> Response {
    let mut req = req;
    req.extensions_mut().insert(Principal {
        subject: Some("u1".into()),
        tenant: Some("acme".into()),
        roles,
    });
    next.run(req).await
}

#[tokio::test]
async fn custom_policy_honours_upstream_principal() {
    let repo = Arc::new(InMemoryRepository::<Product>::new());
    let now = Utc::now();

    // Admin principal -> every operation authorized.
    let admin_app = ProductApi::router::<_, AdminOnly>(repo.clone())
        .layer(middleware::from_fn(|req, next| inject_principal(vec!["admin".into()], req, next)));

    let r = request(
        &admin_app,
        "POST",
        "/products",
        Some(sample(Id::<Product>::new(), "Widget", now)),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);

    // Guest principal (no admin role) -> create denied by the policy.
    let guest_app = ProductApi::router::<_, AdminOnly>(repo)
        .layer(middleware::from_fn(|req, next| inject_principal(vec![], req, next)));

    let r = request(
        &guest_app,
        "POST",
        "/products",
        Some(sample(Id::<Product>::new(), "Gadget", now)),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}
