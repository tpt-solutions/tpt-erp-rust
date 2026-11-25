# promo

> OMS promotion-rules Wasm plugin for TPT ERP.

An example computation-only plugin. It reads live stock levels from the host through the
`erp` interface and returns a stock-aware, per-SKU discount: clearance pricing when stock
is high, scarcity pricing when it is low. It performs no I/O, and the host never links
WASI, so the guest is fully sandboxed.

This plugin is excluded from the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build and validate it with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/promo
tpt plugin validate examples/plugins/promo/target/wasm32-unknown-unknown/release/*.wasm
```

## License

Dual-licensed under [MIT](../../../LICENSE-MIT) or [Apache-2.0](../../../LICENSE-APACHE), at your option.
