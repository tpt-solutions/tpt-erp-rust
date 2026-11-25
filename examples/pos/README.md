# pos

> Reference Retail / POS implementation on TPT ERP.

`pos` is a production-shaped point-of-sale engine built on the framework's primitives:

- **Transactions** (`txn`): a `StateMachine`-derived sale lifecycle
  (`Cart → Tendering → Authorized → Captured`, with `Voided` and `Refunded` branches),
  carrying `Money` line items and tax.
- **Split tender** (`tender`) and **drawer reconciliation**.
- **Pricing** (`pricing`) and **loyalty** (`loyalty`), with pricing delegated to the Wasm
  plugin in `examples/plugins/pricing`.
- **Returns** (`returns`).
- **Offline-first sync** (`sync`).

## Test

```bash
cargo test -p pos
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
