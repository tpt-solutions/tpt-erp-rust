# tpt-erp-entity

> Runtime support for `TptEntity` / `TptApi`: the traits the derives generate
> against, plus an in-memory repository you can ship a demo on today.

This crate is the backing library for the
[`tpt-erp-macros`](../tpt-erp-macros/README.md) derives. The derives *emit* code
that implements the traits defined here, so application authors depend on
`tpt-erp-entity` to model and serve entities while the macro crate stays an
implementation detail (it is re-exported for convenience).

## Traits

| Trait | Role |
|-------|------|
| [`EntityTable`](src/entity.rs) | Table name, primary-key column, and the generated [`Filter`] type for an entity. |
| [`Validatable`](src/validation.rs) | `validate(&self) -> Result<(), ValidationError>` — called before insert/update. |
| [`Auditable`](src/audit.rs) | Exposes the four audit columns (`created_at`, `updated_at`, `created_by`, `updated_by`). |
| [`Repository`](src/repository.rs) | The storage backend the generated Axum router talks to. |
| [`AuthPolicy`](src/auth.rs) | RBAC hook invoked before every CRUD operation. |

## Repository

The central abstraction is `Repository<E>`, an async trait with `list`,
`get`, `create`, `replace`, and `delete`. Because the generated router depends
only on this trait, an entity can be served from Postgres, an in-memory map, or
any other backend.

A thread-safe [`InMemoryRepository`] is included and is fully functional — handy
for tests, demos, and the quickstart (no database required). It validates on
write, enforces id-uniqueness as a `Conflict`, and honors the generated filter +
pagination.

```rust
use tpt_erp_entity::{InMemoryRepository, Repository, Validatable};

let repo = InMemoryRepository::<CustomerEntity>::new();
let created = repo.create(customer.clone()).await.unwrap();
let page = repo.list(Default::default(), Default::default()).await.unwrap();
assert_eq!(page.total, 1);
```

### Why SQLx (and not SeaORM)

The workspace standardizes on **SQLx**: compile-time checked queries, first-class
Postgres Row-Level Security integration (see `tpt-erp-tenant`), and no runtime
query builder to learn. A `TptEntity` emits `#[derive(sqlx::FromRow)]` so a mapped
struct is immediately usable with a `PgPool`. `SeaORM` remains viable behind the
`Repository` trait for teams preferring an active-record style.

## Validation

`#[validate(...)]` directives compile into a `ValidationError` when violated:

- `Required` — empty `String` / `None` rejected.
- `Email` — crude `@` + `.` sanity check.
- `Len(min, max)` — string length bounds.
- `Range(min, max)` — numeric bounds.

## Auth

The generated router calls `A::authorize(op, &principal)` before every handler,
where `op` is one of `List` / `Read` / `Create` / `Update` / `Delete`. Supply
your own `AuthPolicy`, or use the permissive [`AllowAll`] for trusted/internal
services and the quickstart. The `Principal` (subject, tenant, roles) is threaded
through request extensions by the `tpt-erp-tenant` middleware.

## Status

Early development (0.1.0). The `InMemoryRepository` and derive-backed flow are
working; SQLx/Postgres backends are planned. APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
