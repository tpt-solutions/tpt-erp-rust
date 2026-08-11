//! # tpt-erp-entity
//!
//! Runtime support for the [`TptEntity`](tpt_erp_macros::TptEntity) and
//! [`TptApi`](tpt_erp_macros::TptApi) derives.
//!
//! The derives generate code that implements the traits defined here:
//!
//! - [`EntityTable`] — database table mapping and the per-entity query [`Filter`].
//! - [`Validatable`] / [`ValidationError`] — a compile-generated validation hook.
//! - [`Auditable`] / [`AuditFields`] — created/updated bookkeeping.
//! - [`Repository`] — the storage abstraction the generated Axum router talks to.
//! - [`AuthPolicy`] — the RBAC hook invoked by the generated router.
//!
//! ## Why SQLx (and not SeaORM)
//!
//! We standardised on **SQLx** rather than SeaORM. SQLx gives us compile-time
//! checked queries, first-class Postgres Row-Level Security integration (see
//! `tpt-erp-tenant`), and no runtime query builder to learn. `TptEntity` emits a
//! `#[derive(sqlx::FromRow)]` so a mapped struct is immediately usable with a
//! SQLx `PgPool`. SeaORM remains a viable alternative for teams that prefer an
//! active-record style; the [`Repository`] trait isolates the storage backend so
//! either can be plugged in.

pub mod audit;
pub mod auth;
pub mod entity;
pub mod repository;
pub mod validation;

#[cfg(feature = "postgres")]
pub mod postgres_repo;

pub use audit::{AuditFields, Auditable};
pub use auth::{AllowAll, AuthError, AuthPolicy, Operation, Principal};
pub use entity::{ApplyFilter, EntityTable, Filter};
pub use repository::{InMemoryRepository, Page, Pagination, Repository, RepositoryError};
pub use validation::{Validatable, ValidationError};

#[cfg(feature = "postgres")]
pub use postgres_repo::PostgresRepository;

/// Re-export of the `TptEntity` and `TptApi` derive macros so a crate only needs
/// to depend on `tpt-erp-entity` to model and serve entities.
pub use tpt_erp_macros::{TptApi, TptEntity};
