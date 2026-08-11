//! Postgres-backed [`Repository`] implementation (feature `postgres`).
//!
//! Entities are stored as a `JSONB` blob keyed by their primary id, so any
//! `#[derive(TptEntity)]` struct works without hand-written SQL. The generated
//! [`ApplyFilter`] is applied in memory after the rows are fetched — fine for
//! reference/moderate volumes; a production deployment can push the predicate down
//! to SQL later without changing the `Repository` contract. Swap this in for
//! [`InMemoryRepository`] to serve the same generated Axum router from Postgres,
//! with Row-Level Security applied per tenant via `tpt-erp-tenant`.

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgPool;

use crate::entity::{ApplyFilter, EntityId, EntityTable};
use crate::repository::{Page, Pagination, Repository, RepositoryError};
use crate::validation::Validatable;

/// A [`Repository`] backed by a Postgres table `(id TEXT PRIMARY KEY, data JSONB, ...)`.
pub struct PostgresRepository<E: EntityTable> {
    pool: PgPool,
    _marker: std::marker::PhantomData<E>,
}

impl<E: EntityTable> PostgresRepository<E> {
    /// Build a repository over an existing `PgPool`.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            _marker: std::marker::PhantomData,
        }
    }

    /// Ensure the backing table exists (id + JSONB payload + audit timestamps).
    /// Idempotent; call once at startup.
    pub async fn create_table(&self) -> Result<(), RepositoryError> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
                id TEXT PRIMARY KEY, \
                data JSONB NOT NULL, \
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            table = E::table_name()
        );
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn fetch_all(&self) -> Result<Vec<E>, RepositoryError>
    where
        E: Clone + DeserializeOwned + Send + Sync,
    {
        let rows = sqlx::query_as::<_, (String, serde_json::Value)>(&format!(
            "SELECT id, data FROM {}",
            E::table_name()
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for (_, data) in rows {
            out.push(
                serde_json::from_value(data)
                    .map_err(|e| RepositoryError::Backend(e.to_string()))?,
            );
        }
        Ok(out)
    }
}

#[async_trait]
impl<E> Repository<E> for PostgresRepository<E>
where
    E: EntityTable + Clone + Validatable + Serialize + DeserializeOwned + Send + Sync + 'static,
    E::Filter: ApplyFilter<E>,
    E::Id: EntityId,
{
    async fn list(
        &self,
        pagination: Pagination,
        filter: E::Filter,
    ) -> Result<Page<E>, RepositoryError> {
        let mut matched = self.fetch_all().await?;
        matched.retain(|e| filter.matches(e));
        matched.sort_by_key(|e| e.id());
        let total = matched.len() as u64;
        let start = pagination.offset() as usize;
        let end = (start + pagination.limit() as usize).min(matched.len());
        let items = if start >= matched.len() {
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
        let data: Option<serde_json::Value> = sqlx::query_scalar(&format!(
            "SELECT data FROM {} WHERE {} = $1",
            E::table_name(),
            E::id_column()
        ))
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(e.to_string()))?;
        match data {
            Some(v) => Ok(Some(
                serde_json::from_value(v).map_err(|e| RepositoryError::Backend(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    async fn create(&self, entity: E) -> Result<E, RepositoryError> {
        entity.validate()?;
        let data =
            serde_json::to_value(&entity).map_err(|e| RepositoryError::Backend(e.to_string()))?;
        let res = sqlx::query(&format!(
            "INSERT INTO {} (id, data) VALUES ($1, $2) ON CONFLICT ({}) DO NOTHING",
            E::table_name(),
            E::id_column()
        ))
        .bind(entity.id().to_string())
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::Conflict(format!(
                "entity {} already exists",
                entity.id()
            )));
        }
        Ok(entity)
    }

    async fn replace(&self, id: E::Id, entity: E) -> Result<Option<E>, RepositoryError> {
        entity.validate()?;
        let data =
            serde_json::to_value(&entity).map_err(|e| RepositoryError::Backend(e.to_string()))?;
        let res = sqlx::query(&format!(
            "UPDATE {} SET data = $2, updated_at = now() WHERE {} = $1",
            E::table_name(),
            E::id_column()
        ))
        .bind(id.to_string())
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(e.to_string()))?;
        if res.rows_affected() == 0 {
            Ok(None)
        } else {
            Ok(Some(entity))
        }
    }

    async fn delete(&self, id: E::Id) -> Result<bool, RepositoryError> {
        let res = sqlx::query(&format!(
            "DELETE FROM {} WHERE {} = $1",
            E::table_name(),
            E::id_column()
        ))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(e.to_string()))?;
        Ok(res.rows_affected() > 0)
    }
}
