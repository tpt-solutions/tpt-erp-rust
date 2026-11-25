# server

> Reference Axum server wiring multi-tenancy and the ledger into one multi-tenant API.

`server` combines `tpt-erp-tenant` (tenant identification, JWT authentication, Postgres
RLS context) with `tpt-erp-ledger` (append-only event store, double-entry core). It is
the reference for how a production-shaped service composes the framework crates.

## Run

```bash
TPT_JWT_SECRET=<your-secret> cargo run -p server
```

Set `TPT_JWT_SECRET` to enforce real authentication. Without it, requests are not
checked against a signing key, so do not expose the server in that mode.

## Test

```bash
cargo test -p server
```

The integration tests in `tests/integration.rs` cover the end-to-end path from an API
transaction to a ledger entry, including tenant isolation.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
