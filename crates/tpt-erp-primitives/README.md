# tpt-erp-primitives

> The type-level safety layer of TPT ERP. Invalid states and cross-entity
> mistakes become **compile errors** instead of runtime bugs.

`tpt-erp-primitives` provides zero-cost domain-modeling abstractions shared by
every other crate in the workspace. A `Money<Usd>` can never be added to a
`Money<Eur>`, and an `Id<Product>` can never be passed where an `Id<User>` is
expected. These guarantees live in the Rust type system, so they cost nothing at
runtime.

## What's inside

| Type | Purpose |
|------|---------|
| [`Money<C>`](src/money.rs) | A precise [`rust_decimal`] amount tagged with a [`Currency`] marker `C`. Cross-currency arithmetic does not compile. |
| [`Id<T>`](src/id.rs) | A UUID wrapped with an entity marker `T`. `Id<User>` ≠ `Id<Product>`. |
| [`IntId<T>`](src/id.rs) | Same idea backed by an `i64` for serial/sequence columns. |
| `StateMachine` | A derive macro (re-exported from `tpt-erp-macros`) that generates transition-checked state enums. |
| `Currency` markers | `Usd`, `Eur`, `Gbp`, `Jpy` — zero-sized types carrying ISO code, symbol, and minor-unit count. |

The crate is dependency-light (`rust_decimal`, `serde`, `uuid`, `thiserror`) and
pulls in no async runtime, so it compiles anywhere the rest of the framework
does — including the WASM frontend.

## Money

```rust
use tpt_erp_primitives::{Money, Usd, Eur};

let price = Money::<Usd>::from_major(19);
let qty = 3u32;
let total = price * rust_decimal::Decimal::from(qty); // $57

// Currency is part of the type, so this is rejected at compile time:
// let _ = price + Money::<Eur>::from_major(5); // ❌ mismatched currencies
```

`Money` also provides:

- `allocate(&[u64])` — split an amount across ratios with the
  largest-remainder method so the parts sum back exactly to the original.
- `round(strategy)` — scale to the currency's minor units (USD → 2, JPY → 0).
- serde support that **rejects** a JSON payload whose `currency` field does not
  match `C` (see `src/money.rs` tests).

## Strong identifiers

```rust
use tpt_erp_primitives::{Id, Entity};

struct User;
impl Entity for User {}

let id: Id<User> = Id::new();          // random v4 UUID
let same = Id::<User>::parse(&id.as_str()).unwrap();
assert_eq!(id, same);
```

`Id<T>` is `Copy`, `Ord`, `Hash`, and serializes to a plain UUID string. The
marker `T: Entity` is a zero-cost phantom, so there is no runtime overhead —
just compiler-checked routing.

## State machines

```rust
use tpt_erp_primitives::StateMachine;

#[derive(StateMachine, Debug, Clone, Copy, PartialEq)]
#[state_machine(transitions(
    Draft => Confirmed,
    Confirmed => Shipped,
    Confirmed => Cancelled,
))]
enum OrderStatus { Draft, Confirmed, Shipped, Cancelled }

let s = OrderStatus::Draft;
assert!(s.can_transition(OrderStatus::Confirmed));
let s = s.transition(OrderStatus::Cancelled).unwrap_err(); // ❌ not allowed
```

The derive generates `can_transition(&self, to)` and
`transition(self, to) -> Result<Self, OrderStatusTransitionError>`. Illegal
transitions are caught early, and the error carries the `from`/`to` pair.

## Status

This crate is the most mature in the workspace and is used by `tpt-erp-ledger`,
`tpt-erp-entity`, `tpt-erp-tenant`, and `tpt-erp-frontend`. APIs are stabilizing
but may still change between `0.x` releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
