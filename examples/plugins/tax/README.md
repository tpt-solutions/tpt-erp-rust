# tax

> Jurisdiction sales-tax tier Wasm plugin for TPT ERP.

An example computation-only plugin. It reads an account balance through the `erp`
interface and chooses a sales-tax tier for a jurisdiction, such as EU reduced, standard,
or exempt. It performs no I/O, and the host never links WASI, so the plugin can be
hot-swapped per client without a server restart.

This plugin is excluded from the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build and validate it with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/tax
tpt plugin validate examples/plugins/tax/target/wasm32-unknown-unknown/release/*.wasm
```

## License

Dual-licensed under [MIT](../../../LICENSE-MIT) or [Apache-2.0](../../../LICENSE-APACHE), at your option.
