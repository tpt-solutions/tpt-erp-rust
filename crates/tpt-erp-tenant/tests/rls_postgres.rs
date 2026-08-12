#![cfg(feature = "sqlx")]

//! Verifies the consolidated tenant-scoping query (`SET_TENANT_QUERY` = `set_config`) works
//! against a real Postgres instance. Skipped unless `TPT_TEST_POSTGRES_URL` points at a
//! live database, so a default checkout without Postgres still passes CI.
//!
//! This directly addresses the earlier divergence between the raw `SET LOCAL app.tenant_id
//! = '<uuid>'` string form and a parameterized `SET LOCAL app.tenant_id = $1` form: the
//! latter does not accept bind parameters reliably across drivers, whereas `set_config`
//! does. We confirm here that the chosen query binds and actually scopes the session.

use tpt_erp_tenant::rls::SET_TENANT_QUERY;

async fn test_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("TPT_TEST_POSTGRES_URL").ok()?;
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .ok()
}

#[tokio::test]
async fn set_tenant_query_binds_and_scopes_session() {
    let Some(pool) = test_pool().await else {
        eprintln!("skipping: TPT_TEST_POSTGRES_URL not set / unreachable");
        return;
    };

    let tenant = "11111111-1111-1111-1111-111111111111";
    let mut tx = sqlx::Acquire::begin(&pool).await.unwrap();
    sqlx::query(SET_TENANT_QUERY)
        .bind(tenant)
        .execute(&mut *tx)
        .await
        .expect("set_config should accept the bound tenant param");
    let got: (String,) = sqlx::query_as("SELECT current_setting('app.tenant_id')")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(got.0, tenant, "session GUC must reflect the scoped tenant");
    tx.rollback().await.unwrap();
}
