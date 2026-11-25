# pos-ui

> Leptos cashier-terminal view for the POS reference ERP: live transaction state and offline/online indicator.

`pos-ui` is the front-end mirror of the register. It reuses the strong-ID types from
`tpt_erp_primitives`, and its `TxnStage` mirrors the server's `pos::txn::TxnStatus`
state machine to render live status badges.

## Build

For the browser, build with Trunk and the `wasm32-unknown-unknown` target. To check the
host build:

```bash
cargo check -p pos-ui
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
