//! # oms — reference E-commerce / OMS implementation on TPT ERP.
//!
//! This crate is the Sprint D reference: a production-shaped commerce engine built
//! entirely on the framework's primitives, with no shortcuts that would not survive
//! contact with real load:
//!
//! - [`catalog`] — `TptEntity`/`TptApi` **Product** and **Order** CRUD with
//!   role-differentiated [`AuthPolicy`](tpt_erp_entity::AuthPolicy) (customer vs.
//!   staff), pagination, filtering, and RBAC.
//! - [`reservation`] — an event-sourced, per-SKU-sharded **reservation engine** with
//!   TTL auto-release via `tpt-erp-cache`, and structural oversell prevention.
//! - [`saga`] — the **order saga** (`Reserve -> Pay -> Fulfill -> Ship`) as a
//!   hand-rolled compensating-transaction orchestrator on `tpt-erp-bus`; the `Pay`
//!   step posts a real, balanced double-entry transaction through `gl::journal`.
//! - [`promo`] — the **Wasm promo plugin** host glue: a `HostContext` that exposes
//!   live stock, and a [`PromoEngine`] that runs the `examples/plugins/promo` guest.
//! - [`checkout`] — Axum wiring that mounts the catalog CRUD routers and a
//!   `/checkout` handler running the full saga with the promo discount applied.

pub mod catalog;
pub mod checkout;
pub mod promo;
pub mod reservation;
pub mod returns;
pub mod saga;

pub use catalog::{
    CustomerAuth, OrderApi, OrderRow, OrderStatus, ProductApi, ProductRow, StaffAuth,
};
pub use checkout::{CheckoutLine, CheckoutOutcome, OmsApp, OmsError};
pub use promo::{PromoEngine, PromoHost};
pub use reservation::{ReservationEngine, ReservationError, demo_tenant};
pub use returns::{ReturnError, Rma, RmaLine, RmaProcessor, RmaRow, RmaStatus};
pub use saga::{OrderSaga, SagaLine, SagaOutcome, SagaStage};
