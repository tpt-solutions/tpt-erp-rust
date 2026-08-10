//! Database table mapping and the per-entity query `Filter`.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The set of bounds an entity's primary-key type must satisfy so the generated
/// Axum router can extract it from a path / serialise it in JSON.
pub trait EntityId:
    Clone
    + Send
    + Sync
    + std::fmt::Debug
    + Ord
    + Eq
    + std::hash::Hash
    + Serialize
    + DeserializeOwned
    + std::str::FromStr
    + std::fmt::Display
{
}

impl<T> EntityId for T where
    T: Clone
        + Send
        + Sync
        + std::fmt::Debug
        + Ord
        + Eq
        + std::hash::Hash
        + Serialize
        + DeserializeOwned
        + std::str::FromStr
        + std::fmt::Display
{
}

/// A queryable entity. The derive implements this for every `#[derive(TptEntity)]`
/// struct, recording the SQL table name and the generated [`Filter`] type.
pub trait EntityTable: Sized {
    /// The SQL table (or view) this entity is persisted in.
    fn table_name() -> &'static str;

    /// The primary-key column name. Defaults to `"id"` when not overridden.
    fn id_column() -> &'static str {
        "id"
    }

    /// The strongly-typed primary key.
    type Id: EntityId;

    /// The all-optional filter produced by the derive for list/search endpoints.
    type Filter: Filter;

    /// Extract the primary key from an instance.
    fn id(&self) -> Self::Id;
}

/// Marker bound for the generated per-entity filter struct.
///
/// Filters are all-`Option` field bags the list endpoint deserialises from the
/// query string. They also implement [`ApplyFilter`] so the in-memory repository
/// (and any backend that wants to) can match entities without hand-written code.
pub trait Filter: Default + DeserializeOwned + Send + Sync + std::fmt::Debug + Clone {}

/// Implemented by the generated filter so a backend can test membership.
///
/// Equality semantics only (each `Some(field)` must equal the entity's field);
/// more expressive predicates are the backend's responsibility.
pub trait ApplyFilter<E>: Send + Sync {
    /// Whether `entity` satisfies this filter.
    fn matches(&self, entity: &E) -> bool;
}
