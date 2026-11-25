# dispatch

> Stop-scoring dispatch Wasm plugin for TPT ERP.

An example computation-only plugin. It reads a stop's demand from the host through the
`erp` interface and returns a dispatch priority score that blends demand with the routing
weight, such as distance. It performs no I/O, and the host never links WASI, so the guest
is fully sandboxed.

This plugin is excluded from the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build and validate it with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/dispatch
tpt plugin validate examples/plugins/dispatch/target/wasm32-unknown-unknown/release/*.wasm
```

## License

Dual-licensed under [MIT](../../../LICENSE-MIT) or [Apache-2.0](../../../LICENSE-APACHE), at your option.
