# mes-ui

> Leptos operator UI for the MES reference ERP: WIP and QC entry, plus a live OEE dashboard.

`mes-ui` is the front-end mirror of the shop-floor operator console. It reuses
`tpt_erp_primitives::StateMachine` to model the WIP lifecycle in the browser, so the same
transition graph that guards the `mes` server engine also guards the operator's buttons.

## Run

Serve the app with Trunk (see `Trunk.toml`):

```bash
cd examples/mes-ui
trunk serve
```

To check the host build:

```bash
cargo check -p mes-ui
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
