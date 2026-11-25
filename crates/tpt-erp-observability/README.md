# tpt-erp-observability

> Structured logging and Prometheus metrics for TPT ERP services.

`tpt-erp-observability` gives every TPT ERP service the same two building blocks:

- **Tracing**: `init_tracing` installs a `tracing` subscriber driven by the `RUST_LOG`
  environment variable.
- **Metrics**: `install_metrics` registers the Prometheus recorder globally, and
  `metrics_router` exposes the scrape endpoint on an Axum router.

## Usage

```rust
use tpt_erp_observability::{init_tracing, install_metrics, metrics_router};

fn main() {
    init_tracing();
    install_metrics();

    let app = axum::Router::new().merge(metrics_router());
    // serve `app` as usual
}
```

Set the log level at runtime, for example `RUST_LOG=info,tpt_erp_ledger=debug`.

## Status

This crate is internal to the workspace (`publish = false`).

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
