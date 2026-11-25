# tms

> Reference Fleet / TMS implementation on TPT ERP.

`tms` is a production-shaped transportation-management engine built on the framework's
primitives:

- **Geofencing** (`geo`): circle and polygon containment, haversine distance, and zone
  entry/exit events for the event bus.
- **GPS ingestion** (`ingest`): a transport-agnostic, back-pressured pipeline.
- **Route optimization** (`route`): seed tour with 2-opt refinement.
- **Driver hours-of-service** (`hos`): a driver HOS state machine.
- **Dispatch** (`dispatch`): delegated to the Wasm dispatch plugin in `examples/plugins/dispatch`.

## Test

```bash
cargo test -p tms
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
