# gl

> Reference Accounting / General Ledger implementation on TPT ERP.

`gl` is a double-entry accounting reference built on the framework's event-sourced
ledger, with no global row locks and replayable read models.

- **Chart of accounts** (`coa`): account types and normal balances.
- **Journal** (`journal`): multi-currency, event-sourced postings. Legs are sharded by
  account, so concurrent postings to different accounts do not block one another.
- **FX revaluation** (`fx`) and **tax** (`tax`) handling.
- **Consolidation** (`consolidation`) and **period-end close** (`close`).
- **Financial reporting** (`reporting`): CQRS read models for trial balance and balance
  sheet.

## Test

```bash
cargo test -p gl
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
