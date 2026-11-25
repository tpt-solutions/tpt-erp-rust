//! PostgreSQL-backed [`EventBus`] + [`JobQueue`].
//!
//! Enable with the `postgres` feature. Events are appended to the
//! `tpt_bus_messages` table. Each subscription is a durable consumer whose
//! position is stored in `tpt_bus_consumers`, keyed by the subscribed subject,
//! so the same database already used for ledger state also carries the bus.
//!
//! ## Guarantees
//!
//! - **Ordering.** Publishers serialize on a transaction-level advisory lock, so a
//!   row's `seq` becomes visible in order. A consumer therefore never skips a row
//!   that commits later with a lower sequence number.
//! - **At-least-once.** A consumer's stored position only advances past a message
//!   once that message is acknowledged. Messages that were delivered but not
//!   acknowledged are redelivered the next time a subscription on the same subject
//!   is opened (for example after a restart). Redelivery *within* a live
//!   subscription is not implemented.
//! - **Fan-out.** Every subscription receives every matching message. Competing
//!   workers for one job type are not load-balanced yet.

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::stream;
use parking_lot::Mutex;
use sqlx::{PgPool, Row};

use crate::{AckFn, BusError, EventBus, JobQueue, Message, MessageStream};

/// Advisory-lock key that serializes publishers ("tpt_bus" as bytes).
const PUBLISH_LOCK_KEY: i64 = 0x7470_745f_6275_73;
/// How long to wait before polling again when there is nothing new.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Back-off after a database error before the next attempt.
const ERROR_BACKOFF: Duration = Duration::from_secs(1);
/// Maximum rows fetched per poll.
const BATCH_SIZE: i64 = 128;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tpt_bus_messages (
    seq          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    subject      TEXT NOT NULL,
    payload      BYTEA NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS tpt_bus_consumers (
    name   TEXT PRIMARY KEY,
    cursor BIGINT NOT NULL DEFAULT 0
);
";

fn backend_err(e: impl std::fmt::Display) -> BusError {
    BusError::Backend(e.to_string())
}

/// PostgreSQL-backed bus. Cheap to clone the pool handle; share via `Arc` as needed.
pub struct PostgresBus {
    pool: PgPool,
}

impl PostgresBus {
    /// Connect to `url` and create the bus tables if they do not exist.
    pub async fn connect(url: &str) -> Result<Self, BusError> {
        let pool = PgPool::connect(url).await.map_err(backend_err)?;
        Self::from_pool(pool).await
    }

    /// Use an existing pool and create the bus tables if they do not exist.
    pub async fn from_pool(pool: PgPool) -> Result<Self, BusError> {
        sqlx::raw_sql(SCHEMA)
            .execute(&pool)
            .await
            .map_err(backend_err)?;
        Ok(Self { pool })
    }

    /// Open a durable subscription on `subject` (exact, or `prefix.>`).
    async fn open(&self, subject: &str) -> Result<MessageStream, BusError> {
        let sub_err = |e: sqlx::Error| BusError::Subscribe(e.to_string());
        sqlx::query(
            "INSERT INTO tpt_bus_consumers (name, cursor) VALUES ($1, 0) \
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(subject)
        .execute(&self.pool)
        .await
        .map_err(sub_err)?;
        let cursor: i64 =
            sqlx::query_scalar("SELECT cursor FROM tpt_bus_consumers WHERE name = $1")
                .bind(subject)
                .fetch_one(&self.pool)
                .await
                .map_err(sub_err)?;

        let (exact, prefix) = match subject.strip_suffix('>') {
            Some(p) => (p.to_string(), true),
            None => (subject.to_string(), false),
        };
        let tracker = Tracker {
            committed: cursor,
            delivered: cursor,
            outstanding: BTreeSet::new(),
        };
        let state = Cursor {
            pool: self.pool.clone(),
            name: subject.to_string(),
            exact,
            prefix,
            from: cursor,
            buffer: VecDeque::new(),
            tracker: Arc::new(Mutex::new(tracker)),
        };
        Ok(run(state))
    }
}

/// Tracks delivery and acknowledgement for one subscription.
///
/// The durable cursor may only sit just below the lowest unacknowledged sequence
/// number, so that a restart redelivers everything that was not acknowledged.
#[derive(Default)]
struct Tracker {
    /// Position already persisted (or equal to it once the update is issued).
    committed: i64,
    /// Highest sequence number handed to the application.
    delivered: i64,
    /// Sequence numbers handed out but not yet acknowledged.
    outstanding: BTreeSet<i64>,
}

impl Tracker {
    /// Record an acknowledgement and return the new cursor if it moved forward.
    fn ack(&mut self, seq: i64) -> Option<i64> {
        self.outstanding.remove(&seq);
        let target = match self.outstanding.first() {
            Some(&lowest) => lowest - 1,
            None => self.delivered,
        };
        if target > self.committed {
            self.committed = target;
            Some(target)
        } else {
            None
        }
    }
}

/// Per-subscription state driven by the message stream.
struct Cursor {
    pool: PgPool,
    name: String,
    exact: String,
    prefix: bool,
    /// Last sequence number fetched from the database.
    from: i64,
    buffer: VecDeque<(i64, String, Vec<u8>)>,
    tracker: Arc<Mutex<Tracker>>,
}

async fn fetch(state: &Cursor) -> Result<Vec<(i64, String, Vec<u8>)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT seq, subject, payload FROM tpt_bus_messages \
         WHERE seq > $1 \
           AND CASE WHEN $3 THEN starts_with(subject, $2) ELSE subject = $2 END \
         ORDER BY seq LIMIT $4",
    )
    .bind(state.from)
    .bind(&state.exact)
    .bind(state.prefix)
    .bind(BATCH_SIZE)
    .fetch_all(&state.pool)
    .await?;
    rows.iter()
        .map(|r| Ok((r.try_get("seq")?, r.try_get("subject")?, r.try_get("payload")?)))
        .collect()
}

/// Build the acknowledgement callback for one delivered message.
fn ack_for(tracker: Arc<Mutex<Tracker>>, pool: PgPool, name: String, seq: i64) -> AckFn {
    Box::new(move || {
        // Update the in-memory tracker synchronously so ordering is decided at ack time;
        // only the durable write is deferred to the returned future.
        let advanced = tracker.lock().ack(seq);
        let fut: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            if let Some(cursor) = advanced {
                // `cursor < $2` keeps the stored position monotonic even if acks
                // complete out of order.
                let result = sqlx::query(
                    "UPDATE tpt_bus_consumers SET cursor = $2 \
                     WHERE name = $1 AND cursor < $2",
                )
                .bind(&name)
                .bind(cursor)
                .execute(&pool)
                .await;
                if let Err(e) = result {
                    // The message was processed; a failed cursor write only means it may be
                    // redelivered after a restart, which at-least-once already permits.
                    eprintln!("tpt-erp-bus: failed to persist cursor for {name}: {e}");
                }
            }
        });
        fut
    })
}

fn run(state: Cursor) -> MessageStream {
    Box::pin(stream::unfold(state, |mut state| async move {
        loop {
            if let Some((seq, subject, payload)) = state.buffer.pop_front() {
                {
                    let mut t = state.tracker.lock();
                    t.delivered = seq;
                    t.outstanding.insert(seq);
                }
                let ack = ack_for(
                    state.tracker.clone(),
                    state.pool.clone(),
                    state.name.clone(),
                    seq,
                );
                return Some((Message::with_ack(subject, payload, ack), state));
            }
            match fetch(&state).await {
                Ok(rows) if rows.is_empty() => tokio::time::sleep(POLL_INTERVAL).await,
                Ok(rows) => {
                    if let Some(last) = rows.last() {
                        state.from = last.0;
                    }
                    state.buffer.extend(rows);
                }
                Err(e) => {
                    eprintln!("tpt-erp-bus: poll failed for {}: {e}", state.name);
                    tokio::time::sleep(ERROR_BACKOFF).await;
                }
            }
        }
    }))
}

#[async_trait::async_trait]
impl EventBus for PostgresBus {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), BusError> {
        let mut tx = self.pool.begin().await.map_err(backend_err)?;
        // Held until commit: publishers insert one at a time, so sequence numbers commit in order.
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(PUBLISH_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;
        sqlx::query("INSERT INTO tpt_bus_messages (subject, payload) VALUES ($1, $2)")
            .bind(subject)
            .bind(payload)
            .execute(&mut *tx)
            .await
            .map_err(backend_err)?;
        tx.commit().await.map_err(backend_err)
    }

    async fn subscribe(&self, subject: &str) -> Result<MessageStream, BusError> {
        self.open(subject).await
    }
}

#[async_trait::async_trait]
impl JobQueue for PostgresBus {
    async fn enqueue(&self, job_type: &str, payload: &[u8]) -> Result<(), BusError> {
        self.publish(&format!("jobs.{job_type}"), payload).await
    }

    async fn subscribe_jobs(&self, job_type: &str) -> Result<MessageStream, BusError> {
        self.open(&format!("jobs.{job_type}")).await
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests against a real PostgreSQL server. They are `#[ignore]`d so a
    //! plain `cargo test` never needs a database. Run them with:
    //!
    //! ```text
    //! TPT_BUS_TEST_DATABASE_URL=postgres://... cargo test -p tpt-erp-bus --features postgres -- --ignored
    //! ```

    use super::*;
    use futures::StreamExt as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{prefix}.{nanos}")
    }

    async fn bus() -> PostgresBus {
        let url = std::env::var("TPT_BUS_TEST_DATABASE_URL")
            .expect("set TPT_BUS_TEST_DATABASE_URL to run the postgres bus tests");
        PostgresBus::connect(&url).await.expect("connect to test database")
    }

    /// Sequence numbers of every stored row on `subject`, in publish order.
    async fn seqs_of(bus: &PostgresBus, subject: &str) -> Vec<i64> {
        sqlx::query_scalar("SELECT seq FROM tpt_bus_messages WHERE subject = $1 ORDER BY seq")
            .bind(subject)
            .fetch_all(&bus.pool)
            .await
            .unwrap()
    }

    /// Wait until the persisted cursor for `name` reaches `expected` (acks are async).
    async fn wait_for_cursor(bus: &PostgresBus, name: &str, expected: i64) {
        for _ in 0..100 {
            let cursor: i64 = sqlx::query_scalar("SELECT cursor FROM tpt_bus_consumers WHERE name = $1")
                .bind(name)
                .fetch_one(&bus.pool)
                .await
                .unwrap();
            if cursor == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("cursor for {name} never reached {expected}");
    }

    #[tokio::test]
    #[ignore = "requires TPT_BUS_TEST_DATABASE_URL"]
    async fn exact_and_prefix_subscribers_both_receive() {
        let b = bus().await;
        let base = unique("pgtest.orders");
        let mut exact = b.subscribe(&format!("{base}.created")).await.unwrap();
        let mut prefix = b.subscribe(&format!("{base}.>")).await.unwrap();

        b.publish(&format!("{base}.created"), b"evt-1").await.unwrap();

        let e = exact.next().await.unwrap();
        assert_eq!(e.payload, b"evt-1");
        e.ack();
        let p = prefix.next().await.unwrap();
        assert_eq!(p.payload, b"evt-1");
        p.ack();
    }

    #[tokio::test]
    #[ignore = "requires TPT_BUS_TEST_DATABASE_URL"]
    async fn events_arrive_in_publish_order() {
        let b = bus().await;
        let subject = unique("pgtest.order");
        let mut s = b.subscribe(&subject).await.unwrap();
        for i in 0..5u8 {
            b.publish(&subject, &[i]).await.unwrap();
        }
        for i in 0..5u8 {
            let m = s.next().await.unwrap();
            assert_eq!(m.payload, vec![i]);
            m.ack();
        }
    }

    #[tokio::test]
    #[ignore = "requires TPT_BUS_TEST_DATABASE_URL"]
    async fn unacked_message_is_redelivered_after_reopen() {
        let b = bus().await;
        let subject = unique("pgtest.redeliver");
        let mut s = b.subscribe(&subject).await.unwrap();
        for p in [b"a".as_slice(), b"b", b"c"] {
            b.publish(&subject, p).await.unwrap();
        }
        let m1 = s.next().await.unwrap();
        let m2 = s.next().await.unwrap(); // "b" is never acked
        let m3 = s.next().await.unwrap();
        assert_eq!(m2.payload, b"b");
        m1.ack();
        m3.ack();
        // Only "a" sits below the unacked "b", so the durable cursor must stop at a's seq.
        let seqs = seqs_of(&b, &subject).await;
        wait_for_cursor(&b, &subject, seqs[0]).await;
        drop(m2);
        drop(s);

        let mut reopened = b.subscribe(&subject).await.unwrap();
        assert_eq!(reopened.next().await.unwrap().payload, b"b");
        assert_eq!(reopened.next().await.unwrap().payload, b"c");
    }

    #[tokio::test]
    #[ignore = "requires TPT_BUS_TEST_DATABASE_URL"]
    async fn acked_messages_are_not_redelivered() {
        let b = bus().await;
        let subject = unique("pgtest.acked");
        let mut s = b.subscribe(&subject).await.unwrap();
        b.publish(&subject, b"only").await.unwrap();
        let m = s.next().await.unwrap();
        m.ack();
        let seqs = seqs_of(&b, &subject).await;
        wait_for_cursor(&b, &subject, seqs[0]).await;
        drop(s);

        // Reopening must start after "only", so the next thing received is "next".
        let mut reopened = b.subscribe(&subject).await.unwrap();
        b.publish(&subject, b"next").await.unwrap();
        assert_eq!(reopened.next().await.unwrap().payload, b"next");
    }

    #[tokio::test]
    #[ignore = "requires TPT_BUS_TEST_DATABASE_URL"]
    async fn jobs_are_messages_on_jobs_subject() {
        let b = bus().await;
        let job_type = unique("invoice");
        let mut worker = b.subscribe_jobs(&job_type).await.unwrap();
        b.enqueue(&job_type, b"job-42").await.unwrap();
        let m = worker.next().await.unwrap();
        assert_eq!(m.subject, format!("jobs.{job_type}"));
        assert_eq!(m.payload, b"job-42");
        m.ack();
    }
}
