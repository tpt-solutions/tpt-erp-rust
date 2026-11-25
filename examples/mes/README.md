# mes

> Reference Manufacturing / MES implementation on TPT ERP.

`mes` is a stress test and showcase for discrete manufacturing:

- **MRP** (`mrp`): a parallel Material Requirements Planning engine. A Bill-of-Materials
  tree is exploded with rayon, computing gross and net requirements, shortages, and
  critical-path lead time.
- **WIP** (`wip`): a shop-floor work-in-process state machine derived with
  `tpt-erp-primitives`, with prerequisite checks (for example Machined → Assembled).
- **Telemetry and OEE** (`telemetry`, `oee`): CNC/PLC sample ingestion and live
  Availability × Performance × Quality scoring.
- **Defects and rework** (`defects`, `rework`).

## Test and benchmark

```bash
cargo test -p mes
cargo test -p mes --release -- --ignored   # 4,000-part BOM benchmark
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
