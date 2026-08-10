# tpt-erp-frontend

> Leptos WASM frontend for TPT ERP.

A small reference UI demonstrating the framework's headline promise: **the same
type-safe `Money` type that protects the ledger on the server also protects the
shopping cart in the browser.** `tpt-erp-frontend` re-uses
[`tpt-erp-primitives::Money`](../tpt-erp-primitives/README.md) directly, so a
currency mistake is a *compile error* client-side exactly as it is server-side —
one definition, zero drift.

## What it shows

The root `App` component computes a line total in the browser using the shared
`Money<Usd>` type:

```rust
use tpt_erp_primitives::{Money, Usd};

fn line_total(unit: Money<Usd>, qty: u32) -> Money<Usd> {
    unit * rust_decimal::Decimal::from(qty)
}
```

Because `Money<Usd>` is the identical struct the backend uses, the compiler
forbids mixing currencies or treating money as a bare float — the safety that
starts at the domain layer extends all the way to the DOM.

## Build & serve

The crate compiles to WebAssembly (`wasm32-unknown-unknown`). It is set up with
Leptos SSR on the default (host) build so `cargo build` works on a dev machine;
the real browser build uses the `hydrate` feature via
[`trunk`](https://trunkrs.dev/):

```sh
cargo install trunk
trunk build --release
```

To build the WASM artifact directly:

```sh
cargo build -p tpt-erp-frontend --no-default-features --features "hydrate"
```

## Status

Early development (0.1.0). This is a demonstration crate (storefront example),
not a product UI. APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
