# routing

> WMS zone and wave picking-decision Wasm plugin for TPT ERP.

An example computation-only plugin. It reads on-hand stock for each SKU in a pick list
through the `erp` interface and chooses a picking strategy: Zone Picking when stock is
concentrated in one zone, otherwise Wave Picking. It performs no I/O, and the host never
links WASI, so the guest is fully sandboxed.

This plugin is excluded from the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build and validate it with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/routing
tpt plugin validate examples/plugins/routing/target/wasm32-unknown-unknown/release/*.wasm
```

## License

Dual-licensed under [MIT](../../../LICENSE-MIT) or [Apache-2.0](../../../LICENSE-APACHE), at your option.
