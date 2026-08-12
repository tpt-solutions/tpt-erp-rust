//! In-process reference implementation of the bus/job contracts.
//!
//! Uses `tokio::sync::mpsc` channels; a publish fans out to every
//! subscriber whose subject matches (exact or `>`-prefix). No external
//! service required — ideal for tests and single-node local runs.

use std::collections::HashMap;
use parking_lot::Mutex;

use tokio::sync::mpsc;

use crate::{BusError, EventBus, JobQueue, Message, MessageStream};

fn subject_matches(sub: &str, published: &str) -> bool {
    if sub == published {
        return true;
    }
    if let Some(prefix) = sub.strip_suffix('>') {
        return published.starts_with(prefix);
    }
    false
}

#[derive(Default)]
struct Inner {
    subs: HashMap<String, Vec<mpsc::Sender<Message>>>,
}

/// In-memory [`EventBus`] + [`JobQueue`].
#[derive(Default)]
pub struct InMemoryBus {
    inner: Mutex<Inner>,
}

impl InMemoryBus {
    /// Create an empty in-memory bus.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl EventBus for InMemoryBus {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), BusError> {
        let msg = Message::without_ack(subject, payload.to_vec());
        // Snapshot the matching senders under the lock, then drop the lock *before* awaiting
        // so the future stays `Send` and a slow subscriber applies backpressure.
        let targets: Vec<mpsc::Sender<Message>> = {
            let inner = self.inner.lock();
            inner
                .subs
                .iter()
                .filter(|(sub, _)| subject_matches(sub, subject))
                .flat_map(|(_, senders)| senders.iter().cloned())
                .collect()
        };
        for tx in targets {
            // Apply backpressure instead of silently dropping: if a subscriber's buffer is
            // full we wait (and propagate a closed-channel error) rather than lose the event.
            tx.send(msg.clone())
                .await
                .map_err(|_| BusError::Backend("subscriber channel closed".to_string()))?;
        }
        Ok(())
    }

    async fn subscribe(&self, subject: &str) -> Result<MessageStream, BusError> {
        let (tx, rx) = mpsc::channel(64);
        self.inner
            .lock()
            .subs
            .entry(subject.to_string())
            .or_default()
            .push(tx);
        // Bridge the receiver into a `futures::Stream` independent of the
        // tokio/futures Stream-impl coupling.
        let s =
            futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) });
        Ok(Box::pin(s))
    }
}

#[async_trait::async_trait]
impl JobQueue for InMemoryBus {
    async fn enqueue(&self, job_type: &str, payload: &[u8]) -> Result<(), BusError> {
        self.publish(&format!("jobs.{job_type}"), payload).await
    }

    async fn subscribe_jobs(&self, job_type: &str) -> Result<MessageStream, BusError> {
        self.subscribe(&format!("jobs.{job_type}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt as _, StreamExt as _};

    #[tokio::test]
    async fn publish_reaches_exact_and_prefix_subscribers() {
        let bus = InMemoryBus::new();
        let mut exact = bus.subscribe("orders.created").await.unwrap();
        let mut prefix = bus.subscribe("orders.>").await.unwrap();
        let mut other = bus.subscribe("invoices.paid").await.unwrap();

        bus.publish("orders.created", b"evt-1").await.unwrap();

        // Exact + prefix receive; unrelated subject does not.
        let e = exact.next().await.unwrap();
        assert_eq!(e.payload, b"evt-1");
        let p = prefix.next().await.unwrap();
        assert_eq!(p.payload, b"evt-1");
        // `other` should have nothing; poll briefly.
        assert!(other.next().now_or_never().is_none());
    }

    #[tokio::test]
    async fn jobs_are_just_messages_on_jobs_subject() {
        let bus = InMemoryBus::new();
        let mut worker = bus.subscribe_jobs("invoice").await.unwrap();
        bus.enqueue("invoice", b"job-42").await.unwrap();
        let m = worker.next().await.unwrap();
        assert_eq!(m.subject, "jobs.invoice");
        assert_eq!(m.payload, b"job-42");
    }
}
