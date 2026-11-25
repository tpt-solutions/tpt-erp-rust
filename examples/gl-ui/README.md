# gl-ui

> Leptos operator UI for the GL reference ERP: journal-entry and trial-balance views.

`gl-ui` is the front-end mirror of the accounting console. It reuses the same `gl`
journal engine that powers the server, so the UI and the API share one source of truth
for postings and reports.

## Build

For the browser, build the crate as WebAssembly with [Trunk](https://trunkrs.dev) and the
`wasm32-unknown-unknown` target. To check the host build:

```bash
cargo check -p gl-ui
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
