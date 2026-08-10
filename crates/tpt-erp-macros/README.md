# tpt-erp-macros

> Code generation that turns plain structs into database-mapped, validated,
> servable ERP entities.

`tpt-erp-macros` provides the procedural macros that do the heavy lifting for
`tpt-erp-entity`. Each macro emits code against the runtime traits defined in
`tpt-erp-entity`, so a consumer only needs to depend on `tpt-erp-entity` (which
re-exports the macros) to model and serve entities.

## Macros

### `#[derive(StateMachine)]`

Generate a transition-checked state enum. Re-exported from `tpt-erp-primitives`
for convenience. See the
[`tpt-erp-primitives` README](../tpt-erp-primitives/README.md) for details.

### `#[derive(TptEntity)]`

Map a struct to a SQL table and add validation, audit fields, and a query
filter.

```rust
use chrono::{DateTime, Utc};
use tpt_erp_entity::{TptEntity, TptApi, AuditFields, Validatable};
use tpt_erp_primitives::{Id, Entity, Usd, Money};

struct Customer;
impl Entity for Customer {}

#[derive(Debug, Clone, TptEntity, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
#[tpt_entity(table = "customers")]
struct CustomerEntity {
    #[id]
    id: Id<Customer>,
    #[validate(required, len(min = 1, max = 200), email)]
    email: String,
    #[validate(range(min = 0, max = 120))]
    age: i32,
    #[audit]
    created_at: DateTime<Utc>,
    #[audit]
    updated_at: DateTime<Utc>,
    #[audit]
    created_by: Option<String>,
    #[audit]
    updated_by: Option<String>,
}
```

The derive generates:

- `EntityTable` — records the table name and primary-key type.
- `Validatable` — compiles the `#[validate(...)]` directives
  (`required`, `email`, `len(min, max)`, `range(min, max)`) into a `validate()`
  hook run before every insert/update.
- `Auditable` — if the struct carries `created_at` / `updated_at` /
  `created_by` / `updated_by`, a `AuditFields` accessor is generated.
- `{Entity}Filter` — an all-optional, serde-friendly query filter (including
  `page` / `per_page`) plus an `ApplyFilter` impl for in-memory matching.

### `#[derive(TptApi)]`

Generate an Axum CRUD router (list, get, create, replace, delete) for a
`TptEntity`, wired to a [`Repository`] and guarded by an [`AuthPolicy`].

```rust
use tpt_erp_entity::{TptApi, Repository, AllowAll, InMemoryRepository};

#[derive(TptApi)]
#[tpt_api(entity = CustomerEntity, path = "/customers")]
struct CustomerApi;

let repo = std::sync::Arc::new(InMemoryRepository::<CustomerEntity>::new());
let app = CustomerApi::router::<_, AllowAll>(repo);
```

The generated router honors pagination + filtering from the query string,
authorizes every operation through the supplied `AuthPolicy`, and translates
[`RepositoryError`]s into proper HTTP status codes (404 / 400 / 409 / 500).

## Notes

- The derived code references `tpt_erp_entity` and (for `TptApi`) `axum`, so
  the consuming crate must depend on them.
- `TptApi` must be derived on a **unit struct** (it carries no data of its own).
- `TptEntity` requires named fields and exactly one `#[id]` field.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
