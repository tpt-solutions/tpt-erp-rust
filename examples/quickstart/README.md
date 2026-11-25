# quickstart

> The 10-minute quickstart: a type-safe, multi-tenant CRUD API with zero hand-written routes.

`quickstart` is the Phase 1 milestone in one file. It combines `tpt-erp-primitives`
(`Money`, strong IDs, `StateMachine`), `TptEntity` (validation, audit fields, query
filter), and `TptApi` (Axum CRUD router with pagination and RBAC) to serve a validated,
audited, paginated, RBAC-guarded API.

## Run

```bash
cargo run -p quickstart
```

The program starts the API and serves a few requests to show the generated routes at
work.

## Generated routes

`TptApi` produces these endpoints for the derived entity:

| Method | Path     | Purpose        |
|--------|----------|----------------|
| GET    | `/`      | List (paged, filterable) |
| GET    | `/:id`   | Fetch one      |
| POST   | `/`      | Create         |
| PUT    | `/:id`   | Update         |
| DELETE | `/:id`   | Delete         |

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
