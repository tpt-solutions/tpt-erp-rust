//! The storage abstraction the generated Axum router talks to.

use std::collections::HashMap;
use parking_lot::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::entity::{ApplyFilter, EntityId, EntityTable};
use crate::validation::Validatable;

/// Errors surfaced by a [`Repository`].
#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("entity not found")]
    NotFound,
    #[error("validation failed: {0}")]
    Validation(#[from] crate::validation::ValidationError),
    #[error("conflict: {0}")]
    Conflict(String),
    /// A backend (e.g. Postgres) error.
    ///
    /// The original error is preserved as the [`std::error::Error::source`], so callers
    /// can downcast to distinguish failure modes (e.g. connection loss from a constraint
    /// violation) rather than only seeing a flattened string.
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Maximum number of rows a single page may request, regardless of the `per_page` query
/// parameter. Guards against a pathological/abusive `per_page` value exhausting memory.
pub const MAX_PER_PAGE: u32 = 1000;

/// Pagination parsed from query parameters (`page`, `per_page`).
#[derive(Debug, Clone, Copy, Default, Serialize, serde::Deserialize)]
pub struct Pagination {
    /// 1-based page index.
    pub page: u32,
    /// Items per page (clamped to `[1, MAX_PER_PAGE]` on read).
    pub per_page: u32,
}

impl Pagination {
    /// SQL `OFFSET`.
    pub fn offset(&self) -> u64 {
        let page = self.page.max(1) as u64;
        let per = self.per_page.clamp(1, MAX_PER_PAGE) as u64;
        (page - 1) * per
    }

    /// SQL `LIMIT`, capped at [`MAX_PER_PAGE`].
    pub fn limit(&self) -> u64 {
        self.per_page.clamp(1, MAX_PER_PAGE) as u64
    }
}

/// A single page of results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    /// The items on this page.
    pub items: Vec<T>,
    /// The requested page index.
    pub page: u32,
    /// The requested page size.
    pub per_page: u32,
    /// Total number of matching rows across all pages.
    pub total: u64,
}

/// Storage contract for an [`EntityTable`].
///
/// The generated Axum router depends only on this trait, so an entity can be
/// served from Postgres (SQLx), an in-memory map, or any other backend.
#[async_trait]
pub trait Repository<E: EntityTable>: Send + Sync + 'static {
    /// List entities matching `filter`, paginated.
    async fn list(
        &self,
        pagination: Pagination,
        filter: E::Filter,
    ) -> Result<Page<E>, RepositoryError>;

    /// Fetch a single entity by id.
    async fn get(&self, id: E::Id) -> Result<Option<E>, RepositoryError>;

    /// Insert a new entity, returning the stored copy (with server-set fields).
    async fn create(&self, entity: E) -> Result<E, RepositoryError>;

    /// Replace an existing entity, returning `None` if the id was absent.
    async fn replace(&self, id: E::Id, entity: E) -> Result<Option<E>, RepositoryError>;

    /// Delete an entity, returning `true` if something was removed.
    async fn delete(&self, id: E::Id) -> Result<bool, RepositoryError>;
}

/// A thread-safe in-memory [`Repository`] — handy for tests, demos, and the
/// 10-minute quickstart (no database required).
///
/// Filtering uses the generated [`ApplyFilter`] impl; pagination is honoured.
pub struct InMemoryRepository<E: EntityTable> {
    store: Mutex<HashMap<E::Id, E>>,
    _marker: std::marker::PhantomData<E>,
}

impl<E: EntityTable> Default for InMemoryRepository<E> {
    fn default() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<E: EntityTable> InMemoryRepository<E> {
    /// Create an empty repository.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl<E> Repository<E> for InMemoryRepository<E>
where
    E: EntityTable + Clone + Validatable + Send + Sync + 'static,
    E::Filter: ApplyFilter<E>,
    E::Id: EntityId,
{
    async fn list(
        &self,
        pagination: Pagination,
        filter: E::Filter,
    ) -> Result<Page<E>, RepositoryError> {
        let guard = self.store.lock();
        let mut matched: Vec<E> = guard
            .values()
            .filter(|e| filter.matches(e))
            .cloned()
            .collect();
        matched.sort_by_key(|e| e.id());
        let total = matched.len() as u64;
        let start = pagination.offset() as usize;
        let end = (start + pagination.limit() as usize).min(matched.len());
        let items: Vec<E> = if start >= matched.len() {
            Vec::new()
        } else {
            matched[start..end].to_vec()
        };
        Ok(Page {
            items,
            page: pagination.page.max(1),
            per_page: pagination.per_page.max(1),
            total,
        })
    }

    async fn get(&self, id: E::Id) -> Result<Option<E>, RepositoryError> {
        Ok(self
            .store
            .lock()
            .get(&id)
            .cloned())
    }

    async fn create(&self, entity: E) -> Result<E, RepositoryError> {
        entity.validate()?;
        let mut guard = self.store.lock();
        match guard.entry(entity.id()) {
            std::collections::hash_map::Entry::Occupied(_) => Err(RepositoryError::Conflict(
                format!("entity {} already exists", entity.id()),
            )),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(entity.clone());
                Ok(entity)
            }
        }
    }

    async fn replace(&self, id: E::Id, entity: E) -> Result<Option<E>, RepositoryError> {
        entity.validate()?;
        let mut guard = self.store.lock();
        match guard.entry(id) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                slot.insert(entity.clone());
                Ok(Some(entity))
            }
            std::collections::hash_map::Entry::Vacant(_) => Ok(None),
        }
    }

    async fn delete(&self, id: E::Id) -> Result<bool, RepositoryError> {
        Ok(self
            .store
            .lock()
            .remove(&id)
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{ApplyFilter, EntityTable, Filter};
    use crate::validation::{Validatable, ValidationError};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestUser {
        id: u32,
        name: String,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct TestUserFilter {
        name: Option<String>,
    }

    impl EntityTable for TestUser {
        fn table_name() -> &'static str {
            "test_users"
        }
        type Id = u32;
        type Filter = TestUserFilter;
        fn id(&self) -> u32 {
            self.id
        }
    }

    impl Validatable for TestUser {
        fn validate(&self) -> Result<(), ValidationError> {
            if self.name.is_empty() {
                return Err(ValidationError::Required("name"));
            }
            Ok(())
        }
    }

    impl Filter for TestUserFilter {}

    impl ApplyFilter<TestUser> for TestUserFilter {
        fn matches(&self, e: &TestUser) -> bool {
            match &self.name {
                Some(n) => e.name == *n,
                None => true,
            }
        }
    }

    #[test]
    fn pagination_clamps_and_offsets() {
        // per_page below 1 clamps to 1; above MAX_PER_PAGE clamps to MAX_PER_PAGE.
        let p = Pagination {
            page: 2,
            per_page: 0,
        };
        assert_eq!(p.limit(), 1);
        assert_eq!(p.offset(), 1);

        let p = Pagination {
            page: 1,
            per_page: MAX_PER_PAGE + 500,
        };
        assert_eq!(p.limit(), MAX_PER_PAGE as u64);

        // page is 1-based: page 2 means offset of exactly one page width.
        let p = Pagination {
            page: 3,
            per_page: 10,
        };
        assert_eq!(p.offset(), 20);
    }

    #[tokio::test]
    async fn in_memory_crud_roundtrip() {
        let repo = InMemoryRepository::<TestUser>::new();
        let u = TestUser {
            id: 1,
            name: "alice".into(),
        };
        let stored = repo.create(u).await.unwrap();
        assert_eq!(stored.name, "alice");

        // Re-creating the same id is a conflict.
        let dup = repo
            .create(TestUser {
                id: 1,
                name: "alice2".into(),
            })
            .await;
        assert!(matches!(dup, Err(RepositoryError::Conflict(_))));

        // get by id.
        assert_eq!(repo.get(1).await.unwrap().unwrap().name, "alice");
        assert!(repo.get(2).await.unwrap().is_none());

        // replace.
        let replaced = repo
            .replace(
                1,
                TestUser {
                    id: 1,
                    name: "alice-renamed".into(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replaced.name, "alice-renamed");
        assert!(repo.replace(99, TestUser { id: 99, name: "x".into() }).await.unwrap().is_none());

        // delete.
        assert!(repo.delete(1).await.unwrap());
        assert!(!repo.delete(1).await.unwrap());
    }

    #[tokio::test]
    async fn in_memory_validation_rejects_invalid() {
        let repo = InMemoryRepository::<TestUser>::new();
        let res = repo
            .create(TestUser {
                id: 1,
                name: String::new(),
            })
            .await;
        assert!(matches!(res, Err(RepositoryError::Validation(_))));
    }

    #[tokio::test]
    async fn in_memory_list_filters_and_paginates() {
        let repo = InMemoryRepository::<TestUser>::new();
        for i in 0..10 {
            repo.create(TestUser {
                id: i,
                name: if i % 2 == 0 { "even".into() } else { "odd".into() },
            })
            .await
            .unwrap();
        }

        // Filter to "even" names -> 5 rows (ids 0,2,4,6,8), sorted by id.
        let page = repo
            .list(
                Pagination {
                    page: 1,
                    per_page: 100,
                },
                TestUserFilter {
                    name: Some("even".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.items.len(), 5);

        // Pagination: page size 2 over the 5 "even" rows.
        let p1 = repo
            .list(
                Pagination {
                    page: 1,
                    per_page: 2,
                },
                TestUserFilter {
                    name: Some("even".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(p1.items.len(), 2);
        assert_eq!(p1.items[0].id, 0);
        assert_eq!(p1.items[1].id, 2);
    }
}
