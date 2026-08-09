//! 10-minute quickstart for TPT ERP.
//!
//! Demonstrates the three founding primitives:
//! - `Money<Currency>` — cross-currency math is a compile error.
//! - `Id<T>` — cross-entity id mixups are a compile error.
//! - `#[derive(StateMachine)]` — invalid workflow transitions are rejected.

use rust_decimal::Decimal;
use tpt_primitives::{Id, Money, StateMachine, Usd};

#[derive(Debug)]
struct Customer;
impl tpt_primitives::Entity for Customer {}

#[derive(Debug)]
struct Order;
impl tpt_primitives::Entity for Order {}

/// Order lifecycle. Backward transitions (e.g. Shipped -> Draft) are impossible:
/// `OrderState::Shipped.transition(OrderState::Draft)` returns an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, StateMachine)]
#[state_machine(transitions(
    Draft => Confirmed,
    Confirmed => Shipped,
))]
enum OrderState {
    Draft,
    Confirmed,
    Shipped,
}

#[derive(Debug)]
struct LineItem {
    product: Id<Order>,
    price: Money<Usd>,
    qty: u32,
}

fn main() {
    // --- Strong IDs -------------------------------------------------------
    let customer: Id<Customer> = Id::new();
    let order: Id<Order> = Id::new();
    println!("customer={customer}");
    println!("order={order}");

    // --- Type-safe money --------------------------------------------------
    let item = LineItem {
        product: order,
        price: Money::<Usd>::from_major(19),
        qty: 3,
    };
    let subtotal = item.price * Decimal::from(item.qty);
    let tax = subtotal * (Decimal::from(8) / Decimal::from(100));
    let total = subtotal + tax;
    println!(
        "order={} item={} subtotal={subtotal}, tax={tax}, total={total}",
        item.product, order
    );

    // The following would NOT compile — currencies are part of the type:
    // let _ = subtotal + Money::<Eur>::from_major(5);

    // --- State machine ----------------------------------------------------
    let mut state = OrderState::Draft;
    println!(
        "state={state:?} can_ship={}",
        state.can_transition(OrderState::Shipped)
    );

    state = state
        .transition(OrderState::Confirmed)
        .expect("Draft -> Confirmed");
    state = state
        .transition(OrderState::Shipped)
        .expect("Confirmed -> Shipped");
    println!("final state={state:?}");

    match state.transition(OrderState::Draft) {
        Ok(_) => println!("unexpected backward transition allowed"),
        Err(e) => println!("rejected backward transition: {e}"),
    }

    println!("customer={customer} ordered {item:?}");
}
