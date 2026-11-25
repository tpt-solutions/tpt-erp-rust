# qc

> MES quality-control tolerance and telemetry-parsing Wasm plugin for TPT ERP.

An example computation-only plugin with two edge-node evaluations:

- `qc`: a dimensional tolerance check (pass or fail against a nominal ± tolerance).
- `telemetry`: parses a raw CNC/PLC line such as `M1,spindle,1234.5` into structured JSON.

It performs no I/O, and the host never links WASI, so the guest is fully sandboxed.

This plugin is excluded from the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build and validate it with the `tpt` CLI:

```bash
tpt plugin build examples/plugins/qc
tpt plugin validate examples/plugins/qc/target/wasm32-unknown-unknown/release/*.wasm
```

## License

Dual-licensed under [MIT](../../../LICENSE-MIT) or [Apache-2.0](../../../LICENSE-APACHE), at your option.
