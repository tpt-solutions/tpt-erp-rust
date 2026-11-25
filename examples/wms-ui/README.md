# wms-ui

> Leptos operator UI for the WMS reference ERP: inventory and picker route planning.

`wms-ui` is the front-end mirror of the warehouse operator view. It reuses the strong-ID
types from `tpt_erp_primitives`, so the same `Id<Bin>` and `Id<Sku>` protections that
guard the server's inventory engine also guard the picker's screen. Its route-planning
math is a dependency-light port of the server's picking strategies.

## Build

Serve the app with Trunk (see `Trunk.toml`):

```bash
cd examples/wms-ui
trunk serve
```

To check the host build:

```bash
cargo check -p wms-ui
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
