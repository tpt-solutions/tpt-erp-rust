# wms

> Reference 3PL / WMS implementation on TPT ERP.

`wms` is both a demo and a stress test of the framework. It builds two of the hardest
real-time logistics subsystems without touching the ledger's core:

- **Inventory** (`inventory`): a real-time, event-sourced engine. Movements are sharded by
  bin, so concurrent bin updates never block on a global lock. On-hand is a CQRS read
  model cached through `tpt-erp-cache`.
- **Picking** (`picking`): picker-path strategies (naive, nearest-neighbor, batch,
  S-shaped) with distance comparison. Picking decisions can be delegated to the Wasm
  routing plugin in `examples/plugins/routing`.
- **IoT ingestion** (`ingest`): decodes RFID and weight frames into back-pressured
  batches on `tpt-erp-bus`. An optional `mqtt` feature bridges `rumqttc`.

## Test and benchmark

```bash
cargo test -p wms
cargo test -p wms -- --ignored   # route benchmark and IoT load test
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
