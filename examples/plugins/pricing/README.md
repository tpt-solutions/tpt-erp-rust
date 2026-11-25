# pricing

> Balance-tiered POS pricing Wasm plugin for TPT ERP.

An example computation-only plugin. It reads an account balance from the host through the
`erp` interface and applies a balance-tiered discount. It performs no I/O, and the host
never links WASI, so the guest is fully sandboxed.

This plugin is excluded from the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build and validate it with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/pricing
tpt plugin validate examples/plugins/pricing/target/wasm32-unknown-unknown/release/*.wasm
```

## License

Dual-licensed under [MIT](../../../LICENSE-MIT) or [Apache-2.0](../../../LICENSE-APACHE), at your option.
