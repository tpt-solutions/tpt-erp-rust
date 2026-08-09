//! The storage abstraction the generated Axum router talks to.

use std::collections::HashMap;
use std::sync::Mutex;

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
    #[error("backend error: {0}")]
    Backend(String),
}

/// Pagination parsed from query parameters (`page`, `per_page`).
#[derive(Debug, Clone, Copy, Default, Serialize, serde::Deserialize)]
pub struct Pagination {
    /// 1-based page index.
    pub page: u32,
    /// Items per page (clamped by the backend if necessary).
    pub per_page: u32,
}

impl Pagination {
    /// SQL `OFFSET`.
    pub fn offset(&self) -> u64 {
        let page = self.page.max(1) as u64;
        let per = self.per_page.max(1) as u64;
        (page - 1) * per
    }

    /// SQL `LIMIT`.
    pub fn limit(&self) -> u64 {
        self.per_page.max(1) as u64
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
        let guard = self.store.lock().expect("in-memory repo lock poisoned");
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
            .expect("in-memory repo lock poisoned")
            .get(&id)
            .cloned())
    }

    async fn create(&self, entity: E) -> Result<E, RepositoryError> {
        entity.validate()?;
        let mut guard = self.store.lock().expect("in-memory repo lock poisoned");
        match guard.entry(entity.id()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                Err(RepositoryError::Conflict(format!(
                    "entity {} already exists",
                    entity.id()
                )))
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(entity.clone());
                Ok(entity)
            }
        }
    }

    async fn replace(&self, id: E::Id, entity: E) -> Result<Option<E>, RepositoryError> {
        entity.validate()?;
        let mut guard = self.store.lock().expect("in-memory repo lock poisoned");
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
            .expect("in-memory repo lock poisoned")
            .remove(&id)
            .is_some())
    }
}
