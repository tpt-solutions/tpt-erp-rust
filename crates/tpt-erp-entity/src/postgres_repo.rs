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
use sqlx::AssertSqlSafe;
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
        // Identifiers are validated by `check_ident`; values are never interpolated.
        sqlx::query(AssertSqlSafe(sql))
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        Ok(())
    }

    async fn fetch_all(&self) -> Result<Vec<E>, RepositoryError>
    where
        E: Clone + DeserializeOwned + Send + Sync,
    {
        let rows = sqlx::query_as::<_, (String, serde_json::Value)>(AssertSqlSafe(format!(
            "SELECT id, data FROM {}",
            check_ident(E::table_name())
        )))
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
        let data: Option<serde_json::Value> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT data FROM {} WHERE {} = $1",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        )))
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
        let res = sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {} (id, data) VALUES ($1, $2) ON CONFLICT ({}) DO NOTHING",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        )))
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
        let res = sqlx::query(AssertSqlSafe(format!(
            "UPDATE {} SET data = $2, updated_at = now() WHERE {} = $1",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        )))
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
        let res = sqlx::query(AssertSqlSafe(format!(
            "DELETE FROM {} WHERE {} = $1",
            check_ident(E::table_name()),
            check_ident(E::id_column())
        )))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        Ok(res.rows_affected() > 0)
    }
}

/// Session setting carrying the active tenant. Must match `tpt-erp-tenant`'s `app.tenant_id`.
pub const TENANT_GUC: &str = "app.tenant_id";

fn backend(e: sqlx::Error) -> RepositoryError {
    RepositoryError::Backend(Box::new(e))
}

/// A tenant-scoped [`Repository`] over Postgres.
///
/// Every operation runs in a transaction whose `app.tenant_id` is set to this repository's
/// tenant. The table carries a `tenant_id` column and a `FORCE`d Row-Level Security policy,
/// so even a query that forgets a tenant filter cannot read or write another tenant's rows.
/// If no tenant is set, the policy matches nothing (fail closed).
///
/// Row-Level Security does not apply to superusers or to roles with `BYPASSRLS`, so the
/// application must connect as a dedicated non-superuser role.
pub struct TenantPostgresRepository<E: EntityTable> {
    pool: PgPool,
    tenant: String,
    _marker: std::marker::PhantomData<E>,
}

impl<E: EntityTable> TenantPostgresRepository<E> {
    /// Build a repository scoped to `tenant`. Rejects an empty tenant.
    pub fn new(pool: PgPool, tenant: impl Into<String>) -> Result<Self, RepositoryError> {
        let tenant = tenant.into();
        if tenant.is_empty() {
            return Err(RepositoryError::Backend(
                "tenant id must not be empty".into(),
            ));
        }
        Ok(Self {
            pool,
            tenant,
            _marker: std::marker::PhantomData,
        })
    }

    /// Create the tenant-scoped table and its isolation policy. Idempotent. Run as the
    /// table owner (a migration role), not as the application role.
    pub async fn create_table(&self) -> Result<(), RepositoryError> {
        let table = check_ident(E::table_name());
        let statements = [
            format!(
                "CREATE TABLE IF NOT EXISTS {table} (\
                    tenant_id TEXT NOT NULL, \
                    id TEXT NOT NULL, \
                    data JSONB NOT NULL, \
                    created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
                    PRIMARY KEY (tenant_id, id))"
            ),
            format!("ALTER TABLE {table} ENABLE ROW LEVEL SECURITY"),
            format!("ALTER TABLE {table} FORCE ROW LEVEL SECURITY"),
            format!("DROP POLICY IF EXISTS {table}_tenant_isolation ON {table}"),
            format!(
                "CREATE POLICY {table}_tenant_isolation ON {table} FOR ALL \
                 USING (tenant_id = current_setting('{TENANT_GUC}', true)) \
                 WITH CHECK (tenant_id = current_setting('{TENANT_GUC}', true))"
            ),
        ];
        for sql in statements {
            sqlx::query(AssertSqlSafe(sql))
                .execute(&self.pool)
                .await
                .map_err(backend)?;
        }
        Ok(())
    }

    /// Begin a transaction with this repository's tenant set (transaction-local).
    async fn begin(&self) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, RepositoryError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
            .bind(&self.tenant)
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        Ok(tx)
    }
}

#[async_trait]
impl<E> Repository<E> for TenantPostgresRepository<E>
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
        let table = check_ident(E::table_name());
        let mut tx = self.begin().await?;
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT data FROM {table} WHERE tenant_id = $1"
        )))
        .bind(&self.tenant)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;

        let mut matched = Vec::with_capacity(rows.len());
        for v in rows {
            let e: E =
                serde_json::from_value(v).map_err(|e| RepositoryError::Backend(Box::new(e)))?;
            if filter.matches(&e) {
                matched.push(e);
            }
        }
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
        let table = check_ident(E::table_name());
        let mut tx = self.begin().await?;
        let data: Option<serde_json::Value> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT data FROM {table} WHERE tenant_id = $1 AND id = $2"
        )))
        .bind(&self.tenant)
        .bind(id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
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
        let table = check_ident(E::table_name());
        let mut tx = self.begin().await?;
        let res = sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {table} (tenant_id, id, data) VALUES ($1, $2, $3) \
             ON CONFLICT (tenant_id, id) DO NOTHING"
        )))
        .bind(&self.tenant)
        .bind(entity.id().to_string())
        .bind(data)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::Conflict(format!(
                "entity {} already exists",
                entity.id()
            )));
        }
        tx.commit().await.map_err(backend)?;
        Ok(entity)
    }

    async fn replace(&self, id: E::Id, entity: E) -> Result<Option<E>, RepositoryError> {
        entity.validate()?;
        let data =
            serde_json::to_value(&entity).map_err(|e| RepositoryError::Backend(Box::new(e)))?;
        let table = check_ident(E::table_name());
        let mut tx = self.begin().await?;
        let res = sqlx::query(AssertSqlSafe(format!(
            "UPDATE {table} SET data = $3, updated_at = now() WHERE tenant_id = $1 AND id = $2"
        )))
        .bind(&self.tenant)
        .bind(id.to_string())
        .bind(data)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        if res.rows_affected() == 0 {
            Ok(None)
        } else {
            Ok(Some(entity))
        }
    }

    async fn delete(&self, id: E::Id) -> Result<bool, RepositoryError> {
        let table = check_ident(E::table_name());
        let mut tx = self.begin().await?;
        let res = sqlx::query(AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE tenant_id = $1 AND id = $2"
        )))
        .bind(&self.tenant)
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
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

    /// Cross-tenant isolation under RLS. Connects as a non-superuser role, because Postgres
    /// exempts superusers and BYPASSRLS roles from policies even with FORCE.
    #[tokio::test]
    async fn tenant_repository_isolates_rows_between_tenants() {
        use sqlx::postgres::PgConnectOptions;
        use std::str::FromStr;

        let Ok(url) = std::env::var("TPT_TEST_POSTGRES_URL") else {
            eprintln!("skipping: TPT_TEST_POSTGRES_URL not set");
            return;
        };
        let Ok(owner) = PgPool::connect(&url).await else {
            eprintln!("skipping: TPT_TEST_POSTGRES_URL unreachable");
            return;
        };

        // Owner-side setup: a non-superuser app role and a fresh table.
        const ROLE: &str = "tpt_rls_app";
        const PASSWORD: &str = "tpt_rls_app_pw";
        sqlx::query(AssertSqlSafe(format!(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{ROLE}') THEN \
             CREATE ROLE {ROLE} LOGIN PASSWORD '{PASSWORD}'; END IF; END $$"
        )))
        .execute(&owner)
        .await
        .unwrap();

        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct TenantItem {
            id: u32,
            name: String,
        }
        #[derive(Debug, Clone, Default, Serialize, Deserialize)]
        struct TenantItemFilter;
        impl EntityTable for TenantItem {
            fn table_name() -> &'static str {
                "test_tenant_items"
            }
            type Id = u32;
            type Filter = TenantItemFilter;
            fn id(&self) -> u32 {
                self.id
            }
        }
        impl Validatable for TenantItem {
            fn validate(&self) -> Result<(), ValidationError> {
                Ok(())
            }
        }
        impl Filter for TenantItemFilter {}
        impl ApplyFilter<TenantItem> for TenantItemFilter {
            fn matches(&self, _: &TenantItem) -> bool {
                true
            }
        }

        sqlx::query("DROP TABLE IF EXISTS test_tenant_items")
            .execute(&owner)
            .await
            .unwrap();
        let admin = TenantPostgresRepository::<TenantItem>::new(owner.clone(), "setup").unwrap();
        admin.create_table().await.unwrap();
        sqlx::query(AssertSqlSafe(format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON test_tenant_items TO {ROLE}"
        )))
        .execute(&owner)
        .await
        .unwrap();

        // Application connection: same host/db, the non-superuser role.
        let app_opts = PgConnectOptions::from_str(&url)
            .unwrap()
            .username(ROLE)
            .password(PASSWORD);
        let app = PgPool::connect_with(app_opts).await.unwrap();

        let acme = TenantPostgresRepository::<TenantItem>::new(app.clone(), "acme").unwrap();
        let globex = TenantPostgresRepository::<TenantItem>::new(app.clone(), "globex").unwrap();

        acme.create(TenantItem {
            id: 1,
            name: "acme-secret".into(),
        })
        .await
        .unwrap();

        // Globex cannot see, overwrite, or delete acme's row, even by id.
        assert!(globex.get(1).await.unwrap().is_none());
        assert_eq!(
            globex
                .list(Pagination::default(), TenantItemFilter)
                .await
                .unwrap()
                .total,
            0
        );
        assert!(
            globex
                .replace(
                    1,
                    TenantItem {
                        id: 1,
                        name: "pwned".into()
                    }
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(!globex.delete(1).await.unwrap());
        assert_eq!(acme.get(1).await.unwrap().unwrap().name, "acme-secret");

        // The same id is independent per tenant.
        globex
            .create(TenantItem {
                id: 1,
                name: "globex-own".into(),
            })
            .await
            .unwrap();
        assert_eq!(globex.get(1).await.unwrap().unwrap().name, "globex-own");
        assert_eq!(acme.get(1).await.unwrap().unwrap().name, "acme-secret");

        // An empty tenant is rejected up front.
        assert!(TenantPostgresRepository::<TenantItem>::new(app.clone(), "").is_err());

        sqlx::query("DROP TABLE IF EXISTS test_tenant_items")
            .execute(&owner)
            .await
            .unwrap();
    }
}
