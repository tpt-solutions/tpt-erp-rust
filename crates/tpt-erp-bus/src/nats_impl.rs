//! NATS JetStream backend for the bus/job contracts.
//!
//! Enable with the `nats` feature. All events are published to a single
//! durable JetStream stream (`tpt-events`, catch-all subject `>`), so the
//! log is persisted and replayable for CQRS rebuilds. Subscriptions are
//! durable pull consumers filtered by subject, giving at-least-once
//! delivery with ack-based redelivery — suitable for background jobs.

use async_nats::jetstream::consumer::AckPolicy;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::stream::Config as StreamConfig;
use async_nats::jetstream::{self};
use futures::StreamExt;

use crate::{BusError, EventBus, JobQueue, Message, MessageStream};

const STREAM: &str = "tpt-events";

/// Build a valid, stable NATS durable consumer name from a subject. NATS durable names may
/// only contain alphanumerics, `-`, `_`, and `.`; wildcard subjects (e.g. `orders.>`) would
/// otherwise produce an invalid name. Replace any disallowed character with `_` and prefix to
/// keep it distinct from stream names.
fn sanitize_subject(subject: &str) -> String {
    subject
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// NATS JetStream-backed [`EventBus`] + [`JobQueue`].
pub struct NatsBus {
    jetstream: jetstream::Context,
}

impl NatsBus {
    /// Connect to a NATS server (e.g. `nats://localhost:4222`) and ensure
    /// the durable `tpt-events` stream exists.
    pub async fn connect(url: &str) -> Result<Self, BusError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BusError::Backend(e.to_string()))?;
        let jetstream = jetstream::new(client);
        jetstream
            .get_or_create_stream(StreamConfig {
                name: STREAM.to_string(),
                subjects: vec![">".to_string()],
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Backend(e.to_string()))?;
        Ok(Self { jetstream })
    }

    async fn stream(&self) -> Result<async_nats::jetstream::stream::Stream, BusError> {
        self.jetstream
            .get_or_create_stream(StreamConfig {
                name: STREAM.to_string(),
                subjects: vec![">".to_string()],
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Backend(e.to_string()))
    }
}

#[async_trait::async_trait]
impl EventBus for NatsBus {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), BusError> {
        let payload = payload.to_vec().into();
        self.jetstream
            .publish(subject.to_string(), payload)
            .await
            .map_err(|e| BusError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn subscribe(&self, subject: &str) -> Result<MessageStream, BusError> {
        let stream = self.stream().await?;
        let consumer = stream
            .get_or_create_consumer(
                STREAM,
                PullConfig {
                    durable_name: Some(format!("sub-{}", sanitize_subject(subject))),
                    filter_subject: subject.to_string(),
                    ack_policy: AckPolicy::Explicit,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BusError::Subscribe(e.to_string()))?;
        let messages = consumer
            .messages()
            .await
            .map_err(|e| BusError::Subscribe(e.to_string()))?;
        let s = messages.then(|res| async move {
            match res {
                Ok(msg) => {
                    let subject = msg.message.subject.to_string();
                    let payload = msg.message.payload.to_vec();
                    // Defer the ack until the *consumer* acknowledges the message (after it
                    // has been processed). Acknowledge-on-receive here previously broke
                    // at-least-once delivery: a crash during processing would lose the event.
                    let ack: crate::AckFn = Box::new(move || {
                        let msg = msg;
                        Box::pin(async move {
                            let _ = msg.ack().await;
                        })
                    });
                    Message::with_ack(subject, payload, ack)
                }
                Err(_) => Message::without_ack(String::new(), Vec::new()),
            }
        });
        Ok(Box::pin(s))
    }
}

#[async_trait::async_trait]
impl JobQueue for NatsBus {
    async fn enqueue(&self, job_type: &str, payload: &[u8]) -> Result<(), BusError> {
        self.publish(&format!("jobs.{job_type}"), payload).await
    }

    async fn subscribe_jobs(&self, job_type: &str) -> Result<MessageStream, BusError> {
        self.subscribe(&format!("jobs.{job_type}")).await
    }
}
