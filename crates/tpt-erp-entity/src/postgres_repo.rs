//! Postgres-backed [`Repository`] implementation (feature `postgres`).
//!
//! Entities are stored as a `JSONB` blob keyed by their primary id, so any
//! `#[derive(TptEntity)]` struct works without hand-written SQL. The generated
//! [`ApplyFilter`] is applied in memory after the rows are fetched — fine for
//! reference/moderate volumes; a production deployment can push the predicate down
//! to SQL later without changing the `Repository` contract. Swap this in for
//! [`InMemoryRepository`] to serve the same generated Axum router from Postgres,
//! with Row-Level Security applied per tenant via `tpt-erp-tenant`.
//!
//! ## SQL identifier safety
//!
//! Table and column names are interpolated into SQL via `format!` (e.g.
//! `format!("SELECT ... FROM {}", check_ident(E::table_name()))`). These identifiers are
//! **not** user input: they are supplied exclusively by the `#[derive(TptEntity)]`
//! macro's `table`/`id` attributes at compile time. Values, by contrast, are
//! always bound via `sqlx::query(...).bind(...)` and never interpolated. The
//! [`check_ident`] guard asserts every interpolated identifier is a safe SQL
//! identifier; if it ever fires, that indicates a macro bug, not attacker
//! control. Should identifiers ever become dynamic, route them through
//! `check_ident` (or proper quoting) before interpolation.

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgPool;

use crate::entity::{ApplyFilter, EntityId, EntityTable};
use crate::repository::{Page, Pagination, Repository, RepositoryError};
use crate::validation::Validatable;

/// Assert `name` is a safe bare SQL identifier (ASCII alphanumerics + `_`).
///
/// See the module docs for why this is a defensive guard rather than a runtime
/// injection boundary. A failing assertion means a `#[derive(TptEntity)]` emitted
/// an invalid identifier and is a programming error in the macro, not attacker
/// input reaching SQL.
fn check_ident(name: &str) -> &str {
    debug_assert!(
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "SQL identifier `{name}` is not a safe bare identifier"
    );
    name
}

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
            table = check_ident(E::table_name())
        );
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn fetch_all(&self) -> Result<Vec<E>, RepositoryError>
    where
        E: Clone + DeserializeOwned + Send + Sync,
    {
        let rows = sqlx::query_as::<_, (String, serde_json::Value)>(&format!(
            "SELECT id, data FROM {}",
            check_ident(E::table_name())
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        let mut out = Vec::with_capacity(rows.len());
        for (_, data) in rows {
            out.push(
                serde_json::from_value(data).map_err(|e| RepositoryError::Backend(Box::new(e)))?,
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
            check_ident(E::table_name()),
            check_ident(E::id_column())
        ))
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        match data {
            Some(v) => Ok(Some(
                serde_json::from_value(v).map_err(|e| RepositoryError::Backend(Box::new(e)))?,
            )),
            None => Ok(None),
        }
    }

    async fn create(&self, entity: E) -> Result<E, RepositoryError> {
        entity.validate()?;
        let data =
            serde_json::to_value(&entity).map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        let res = sqlx::query(&format!(
            "INSERT INTO {} (id, data) VALUES ($1, $2) ON CONFLICT ({}) DO NOTHING",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        ))
        .bind(entity.id().to_string())
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
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
            serde_json::to_value(&entity).map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        let res = sqlx::query(&format!(
            "UPDATE {} SET data = $2, updated_at = now() WHERE {} = $1",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        ))
        .bind(id.to_string())
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        if res.rows_affected() == 0 {
            Ok(None)
        } else {
            Ok(Some(entity))
        }
    }

    async fn delete(&self, id: E::Id) -> Result<bool, RepositoryError> {
        let res = sqlx::query(&format!(
            "DELETE FROM {} WHERE {} = $1",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        ))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod ident_tests {
    use super::check_ident;

    #[test]
    fn accepts_valid_identifiers() {
        assert_eq!(check_ident("orders"), "orders");
        assert_eq!(check_ident("line_items_v2"), "line_items_v2");
    }

    #[test]
    #[should_panic]
    fn rejects_empty_identifier() {
        check_ident("");
    }

    #[test]
    #[should_panic]
    fn rejects_unsafe_identifier() {
        check_ident("orders; DROP TABLE users");
    }
}

#[cfg(test)]
#[cfg(feature = "postgres")]
mod tests {
    use super::*;
    use crate::entity::{ApplyFilter, EntityTable, Filter};
    use crate::repository::Repository;
    use crate::validation::{Validatable, ValidationError};
    use serde::{Deserialize, Serialize};
    use sqlx::postgres::PgPool;

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
            "test_users_pg"
        }
        type Id = u32;
        type Filter = TestUserFilter;
        fn id(&self) -> u32 {
            self.id
        }
    }

    impl Validatable for TestUser {
        fn validate(&self) -> Result<(), ValidationError> {
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

    async fn pool() -> Option<PgPool> {
        let url = std::env::var("TPT_TEST_POSTGRES_URL").ok()?;
        PgPool::connect(&url).await.ok()
    }

    #[tokio::test]
    async fn postgres_crud_roundtrip() {
        let Some(pool) = pool().await else {
            eprintln!("skipping: TPT_TEST_POSTGRES_URL not set / unreachable");
            return;
        };
        // Start from a clean table so the test is repeatable.
        sqlx::query("DROP TABLE IF EXISTS test_users_pg")
            .execute(&pool)
            .await
            .unwrap();
        let repo = PostgresRepository::<TestUser>::new(pool.clone());
        repo.create_table().await.unwrap();

        let stored = repo
            .create(TestUser {
                id: 7,
                name: "bob".into(),
            })
            .await
            .unwrap();
        assert_eq!(stored.name, "bob");

        // Re-creating the same id is a conflict (ON CONFLICT DO NOTHING).
        assert!(matches!(
            repo.create(TestUser {
                id: 7,
                name: "bob2".into(),
            })
            .await,
            Err(RepositoryError::Conflict(_))
        ));

        assert_eq!(repo.get(7).await.unwrap().unwrap().name, "bob");
        assert!(repo.get(8).await.unwrap().is_none());

        assert!(repo.delete(7).await.unwrap());
        assert!(!repo.delete(7).await.unwrap());
    }
}
