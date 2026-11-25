# oms

> Reference E-commerce / OMS implementation on TPT ERP.

`oms` is a production-shaped commerce engine built on the framework's primitives:

- **Catalog** (`catalog`): `TptEntity`/`TptApi` Product and Order CRUD with
  role-differentiated `AuthPolicy` (customer vs. admin).
- **Reservations** (`reservation`): event-sourced stock reservations.
- **Checkout** (`checkout`) and **order saga** (`saga`): reserve → pay → fulfill → ship.
- **Returns** (`returns`).
- **Promotions** (`promo`): delegated to the Wasm promo plugin in `examples/plugins/promo`.

## Test

```bash
cargo test -p oms
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
