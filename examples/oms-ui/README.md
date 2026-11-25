# oms-ui

> Leptos storefront and checkout view for the OMS reference ERP, with live saga-status badges.

`oms-ui` is the front-end mirror of the storefront. It reuses the strong-ID types from
`tpt_erp_primitives`, so a mix-up of two SKUs is a compile error on the customer's screen
as well as on the server. Its `SagaStatus` mirrors the server's order saga.

## Build

For the browser, build with Trunk and the `wasm32-unknown-unknown` target. To check the
host build:

```bash
cargo check -p oms-ui
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
