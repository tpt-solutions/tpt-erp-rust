# tpt-erp-cli

> `tpt` — the command-line interface for working with TPT ERP WebAssembly
> plugins.

The CLI wraps the [`tpt-erp-wasm`](../tpt-erp-wasm/README.md) runtime so plugin
authors can scaffold, build, validate, and execute computation-only plugins
without writing linker or runtime glue by hand.

## Commands

```
tpt plugin new <name>          Scaffold a new computation-only plugin crate
tpt plugin build [path]        Compile + componentize a plugin against the WIT contract
tpt plugin validate <wasm>     Confirm a .wasm satisfies the `plugin` world
tpt plugin run <wasm> <input>  Execute a plugin's `run` with a JSON input string
```

### Scaffold

`tpt plugin new myprice` generates a complete plugin crate: a `Cargo.toml`
(`crate-type = ["cdylib"]`), a `wit/erp.wit` copied from the embedded contract,
and a starter `src/lib.rs` that imports only `erp` and exports `run`. The exact
WIT contract is embedded in the binary, so scaffolding works without locating the
crate on disk.

### Build

`tpt plugin build` cross-compiles for `wasm32-unknown-unknown` (override with
`--target`) and componentizes the core module with `wit-component`, validating it
against the `plugin` world. Output defaults to `<crate>/<crate>.wasm`.

### Validate & run

```sh
tpt plugin validate myprice.wasm
tpt plugin run myprice.wasm '{"sku":"A-1"}' --data host.json --tenant acme
```

`run` loads the plugin into a fresh [`PluginRuntime`] with `RuntimeConfig::default()`
(sandbox limits on), feeds it the JSON `input`, and prints the JSON output. The
optional `--data` file (JSON with `accounts` / `stock` maps) seeds the host
context the plugin queries via `erp`; `--tenant` sets the tenant label reported
to the plugin.

## Example host-data file

```json
{
  "accounts": { "a1": { "major": 3, "minor": 5000 } },
  "stock":    { "s1": 9 }
}
```

## Status

Early development (0.1.0). The `tpt plugin` family is implemented and exercised
by end-to-end tests. More command groups (migrations, tenant tooling) are planned.
APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
