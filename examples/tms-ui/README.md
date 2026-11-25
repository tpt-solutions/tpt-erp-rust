# tms-ui

> Leptos dispatcher live-map and route-plan view for the TMS reference ERP.

`tms-ui` is the front-end mirror of the dispatcher console. It reuses the strong-ID
types from `tpt_erp_primitives`, and its `RouteStage` mirrors the route lifecycle
(seed tour → 2-opt refinement → dispatched) to render live status badges.

## Build

For the browser, build with Trunk and the `wasm32-unknown-unknown` target. To check the
host build:

```bash
cargo check -p tms-ui
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
