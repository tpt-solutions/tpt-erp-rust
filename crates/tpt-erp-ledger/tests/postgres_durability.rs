#![cfg(feature = "postgres")]

//! Verifies that [`PostgresEventStore`] surfaces Postgres mirror failures instead of
//! silently dropping them (the data-loss anti-pattern fixed in `postgres_store.rs`).

use tpt_erp_ledger::postgres_store::PostgresEventStore;
use tpt_erp_ledger::{Event, EventStore, EventStoreError};

fn unreachable_store() -> PostgresEventStore<String> {
    // Lazily build a pool that will fail its first query (no Postgres on this port).
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy("postgres://nouser:nopass@127.0.0.1:1/none")
        .expect("lazy pool build should not connect");
    PostgresEventStore::new(pool)
}

#[tokio::test(flavor = "multi_thread")]
async fn versioned_append_surfaces_postgres_failure() {
    let mut store = unreachable_store();
    let result = store.append_versioned(Event::new("agg1".to_string(), "Created", &"payload").unwrap(), 1);
    assert!(
        matches!(result, Err(EventStoreError::Backend(_))),
        "a Postgres write failure must be surfaced, not silently dropped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn append_counts_durability_failure() {
    let mut store = unreachable_store();
    let _ = store.append(Event::new("agg2".to_string(), "Created", &"payload").unwrap());
    assert_eq!(
        store.durability_failures(),
        1,
        "a failed mirror write must be counted so it can be surfaced in a health check"
    );
}

#[test]
fn backend_error_preserves_typed_source() {
    // A flattened-string Backend variant would lose the underlying cause. Confirm the
    // original error is still reachable via `source()`.
    let sqlx_err = sqlx::Error::PoolTimedOut;
    let backend = EventStoreError::from(sqlx_err);
    let source = std::error::Error::source(&backend).expect("Backend must carry a source");
    assert!(
        source.downcast_ref::<sqlx::Error>().is_some(),
        "Backend source must downcast back to the original sqlx::Error"
    );
}
