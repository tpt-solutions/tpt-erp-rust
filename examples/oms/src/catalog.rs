//! Catalog domain: products and orders exposed as `TptEntity`/`TptApi` CRUD
//! surfaces with role-differentiated RBAC.
//!
//! * [`Product`] and [`Order`] are `#[derive(TptEntity)]` so the macro generates
//!   table mapping, validation, audit fields, and an all-optional [`Filter`].
//! * `#[derive(TptApi)]` turns each into a fully type-safe Axum CRUD router.
//! * Access is gated by two [`AuthPolicy`] implementations — [`StaffAuth`] (full
//!   access) for the back office, and [`CustomerAuth`] (no delete/update of
//!   records) for the storefront.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tpt_erp_entity::{AuthError, AuthPolicy, Operation, TptApi, TptEntity};
use tpt_erp_primitives::{Entity, Id, Money, StateMachine, Usd};

// --- entity markers --------------------------------------------------------

/// A sellable item.
#[derive(Debug)]
pub struct Product;
impl Entity for Product {}

/// A customer placed order.
#[derive(Debug)]
pub struct Order;
impl Entity for Order {}

/// A customer account (used only as a strong id on [`Order`]).
#[derive(Debug)]
pub struct Customer;
impl Entity for Customer {}

/// A line within an [`Order`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderLine {
    pub product: Id<Product>,
    pub sku: String,
    pub qty: u32,
    pub unit_price: Money<Usd>,
}

/// The lifecycle of an order, enforced by a [`StateMachine`].
///
/// `Cart -> Reserved -> Paid -> Fulfilled -> Shipped`, with compensation
/// branches back to `Cancelled` from `Reserved` or `Paid`. Illegal jumps
/// (e.g. `Cart -> Paid`) are rejected at runtime with a typed error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, StateMachine)]
#[state_machine(transitions(
    Cart => Reserved,
    Reserved => Paid,
    Paid => Fulfilled,
    Fulfilled => Shipped,
    Reserved => Cancelled,
    Paid => Cancelled,
))]
pub enum OrderStatus {
    Cart,
    Reserved,
    Paid,
    Fulfilled,
    Shipped,
    Cancelled,
}

/// A catalog product.
#[derive(Clone, Serialize, Deserialize, TptEntity)]
#[tpt_entity(table = "products")]
pub struct ProductRow {
    #[id]
    pub id: Id<Product>,
    #[validate(required, len(min = 1, max = 64))]
    pub sku: String,
    #[validate(required, len(min = 1, max = 256))]
    pub name: String,
    pub price: Money<Usd>,
    pub stock_on_hand: i64,
    #[audit]
    pub created_at: DateTime<Utc>,
    #[audit]
    pub updated_at: DateTime<Utc>,
    #[audit]
    pub created_by: Option<String>,
    #[audit]
    pub updated_by: Option<String>,
}

/// A customer order.
#[derive(Clone, Serialize, Deserialize, TptEntity)]
#[tpt_entity(table = "orders")]
pub struct OrderRow {
    #[id]
    pub id: Id<Order>,
    #[validate(required)]
    pub customer: Id<Customer>,
    pub status: OrderStatus,
    pub lines: Vec<OrderLine>,
    pub total: Money<Usd>,
    #[audit]
    pub created_at: DateTime<Utc>,
    #[audit]
    pub updated_at: DateTime<Utc>,
    #[audit]
    pub created_by: Option<String>,
    #[audit]
    pub updated_by: Option<String>,
}

/// The Axum CRUD router for products (back-office, full access).
#[derive(TptApi)]
#[tpt_api(entity = ProductRow, path = "/products", auth = StaffAuth)]
pub struct ProductApi;

/// The Axum CRUD router for orders (storefront, customer policy).
#[derive(TptApi)]
#[tpt_api(entity = OrderRow, path = "/orders", auth = CustomerAuth)]
pub struct OrderApi;

// --- role-differentiated RBAC ---------------------------------------------

/// Back-office policy: staff may perform every CRUD operation.
pub struct StaffAuth;

impl AuthPolicy for StaffAuth {
    fn authorize(_op: Operation, _principal: &tpt_erp_entity::Principal) -> Result<(), AuthError> {
        Ok(())
    }
}

/// Storefront policy: customers may browse, read, and create, but never delete
/// or mutate existing records (those are staff privileges).
pub struct CustomerAuth;

impl AuthPolicy for CustomerAuth {
    fn authorize(op: Operation, _principal: &tpt_erp_entity::Principal) -> Result<(), AuthError> {
        match op {
            Operation::Delete | Operation::Update => Err(AuthError(op)),
            Operation::List | Operation::Read | Operation::Create => Ok(()),
        }
    }
}

/// Convenience constructor for a new product row.
pub fn new_product(
    sku: impl Into<String>,
    name: impl Into<String>,
    price: Money<Usd>,
    stock_on_hand: i64,
    by: impl Into<String>,
) -> ProductRow {
    let now = Utc::now();
    let by = by.into();
    ProductRow {
        id: Id::new(),
        sku: sku.into(),
        name: name.into(),
        price,
        stock_on_hand,
        created_at: now,
        updated_at: now,
        created_by: Some(by.clone()),
        updated_by: Some(by),
    }
}

/// Convenience constructor for a new order row in the `Cart` state.
pub fn new_order(
    customer: Id<Customer>,
    lines: Vec<OrderLine>,
    total: Money<Usd>,
    by: impl Into<String>,
) -> OrderRow {
    let now = Utc::now();
    let by = by.into();
    OrderRow {
        id: Id::new(),
        customer,
        status: OrderStatus::Cart,
        lines,
        total,
        created_at: now,
        updated_at: now,
        created_by: Some(by.clone()),
        updated_by: Some(by),
    }
}
