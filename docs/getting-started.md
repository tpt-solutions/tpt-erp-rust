# Getting Started

This tutorial builds a type-safe, multi-tenant CRUD API in under ten minutes using the
`#[derive(TptEntity)]` and `#[derive(TptApi)]` macros. It mirrors `examples/quickstart`.

## 1. Prerequisites

```bash
rustup toolchain install 1.97.0
rustup component add rustfmt clippy
# WASM plugins (optional):
rustup target add wasm32-unknown-unknown
```

## 2. Define an entity

```rust
use serde::{Serialize, Deserialize};
use tpt_erp_entity::{TptEntity, TptApi, InMemoryRepository, AllowAll};
use tpt_erp_primitives::{Entity, Id};
use tpt_erp_macros::{TptEntity, TptApi};

impl Entity for Customer {}

#[derive(Debug, Clone, Serialize, Deserialize, TptEntity)]
#[tpt_entity(table = "customers")]
struct Customer {
    #[id]
    id: Id<Customer>,
    #[validate(required, len(min = 1, max = 200), email)]
    email: String,
    #[audit]
    created_at: chrono::DateTime<chrono::Utc>,
    #[audit]
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(TptApi)]
#[tpt_api(entity = Customer, path = "/customers")]
struct CustomerApi;
```

`#[id]` marks the primary key, `#[validate(...)]` generates insert/update guards, and
`#[audit]` feeds the `Auditable` trait (created/updated at/by).

## 3. Mount the generated router

```rust
use std::sync::Arc;
use axum::Router;

let repo = Arc::new(InMemoryRepository::<Customer>::new());
let app: Router = CustomerApi::router::<_, AllowAll>(repo);
```

That single line wires `GET /customers`, `GET /customers/:id`, `POST /customers`,
`PUT /customers/:id`, and `DELETE /customers/:id` with pagination, filtering, validation,
and RBAC (here `AllowAll`). A custom `AuthPolicy` is honoured — supply it as the second
generic and inject a `Principal` from your auth middleware; the generated router never
overwrites a real principal.

## 4. Run it

```rust
# tokio::runtime::Runtime::new().unwrap().block_on(async {
let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
axum::serve(listener, app).await.unwrap();
# });
```

Now `curl -X POST localhost:3000/customers -d '{"id":"...","email":"a@b.com"}' -H
'content-type: application/json'` creates a customer, and `GET /customers?email_contains=b`
uses the extended filter support.

## 5. Next steps

- Swap `InMemoryRepository` for a Postgres-backed `Repository` (implement the
  `Repository` trait; the event store lives behind the `EventStore` trait too).
- Add multi-tenancy with `tpt-erp-tenant` (Postgres RLS via `tenant_rls_middleware`).
- Extend behaviour with a sandboxed WASM plugin (`tpt-erp-cli plugin new`).
