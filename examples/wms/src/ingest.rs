//! High-throughput IoT ingestion pipeline for the WMS.
//!
//! Conveyor sensors and RFID gates emit a firehose of messages. This module turns that
//! firehose into a back-pressured, batched stream of domain events published onto the
//! `tpt-erp-bus` event backbone (so the rest of the system reacts via CQRS projections).
//!
//! The pipeline is **transport-agnostic**: a source is any `Stream<Item = Vec<u8>>`
//! (MQTT, raw TCP, a file, or a synthetic generator for load tests). The decode/transform/
//! publish stages are identical regardless of where bytes came from, which is what makes
//! the throughput testable without a live broker.

use futures::Stream;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use std::time::{Duration, Instant};
use tpt_erp_bus::EventBus;

/// A decoded IoT event from the warehouse floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum IngestEvent {
    /// An RFID gate read: a tag passed `reader`.
    RfidGate { reader: String, tag: String },
    /// A conveyor scale sample: `grams` on `sensor`.
    ConveyorWeight { sensor: String, grams: u32 },
    /// Anything we could not parse; kept so no datum is silently dropped.
    Unparsed(Vec<u8>),
}

#[derive(Debug, Deserialize)]
struct WireEvent {
    #[serde(rename = "type")]
    kind: String,
    reader: Option<String>,
    tag: Option<String>,
    sensor: Option<String>,
    grams: Option<u32>,
}

/// Decode a single raw payload into a domain [`IngestEvent`].
///
/// Wire format is one JSON object per message:
/// `{"type":"rfid","reader":"G1","tag":"T42"}` or
/// `{"type":"weight","sensor":"S7","grams":1234}`. Unparseable bytes become
/// [`IngestEvent::Unparsed`] rather than being dropped.
pub fn decode(payload: &[u8]) -> IngestEvent {
    match serde_json::from_slice::<WireEvent>(payload) {
        Ok(w) => match w.kind.as_str() {
            "rfid" => IngestEvent::RfidGate {
                reader: w.reader.unwrap_or_default(),
                tag: w.tag.unwrap_or_default(),
            },
            "weight" => IngestEvent::ConveyorWeight {
                sensor: w.sensor.unwrap_or_default(),
                grams: w.grams.unwrap_or(0),
            },
            _ => IngestEvent::Unparsed(payload.to_vec()),
        },
        Err(_) => IngestEvent::Unparsed(payload.to_vec()),
    }
}

/// Throughput/correctness counters produced by [`run_pipeline`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestStats {
    /// Number of messages successfully decoded into a domain event.
    pub decoded: u64,
    /// Number of messages that failed to parse.
    pub unparsed: u64,
    /// Number of events published to the bus (after batching).
    pub published: u64,
    /// Wall-clock time the pipeline took.
    pub elapsed: Duration,
}

/// Run the ingestion pipeline to completion.
///
/// * `source` yields raw payloads (one per message).
/// * `bus` receives batched events on the `iot.events` subject.
/// * `batch` is the max events per bus publish (batches cut latency vs. message count).
///
/// Back-pressure is applied naturally: the bounded work is driven by `await`ing `bus`,
/// so a slow consumer throttles the decode stage instead of unbounded buffering.
pub async fn run_pipeline<S>(mut source: S, bus: &dyn EventBus, batch: usize) -> IngestStats
where
    S: Stream<Item = Vec<u8>> + Unpin + Send,
{
    let mut stats = IngestStats::default();
    let start = Instant::now();
    let mut buf: Vec<IngestEvent> = Vec::with_capacity(batch.max(1));

    while let Some(msg) = source.next().await {
        let ev = decode(&msg);
        match &ev {
            IngestEvent::Unparsed(_) => stats.unparsed += 1,
            _ => stats.decoded += 1,
        }
        buf.push(ev);
        if buf.len() >= batch {
            publish_batch(&buf, bus).await;
            stats.published += buf.len() as u64;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        publish_batch(&buf, bus).await;
        stats.published += buf.len() as u64;
    }
    stats.elapsed = start.elapsed();
    stats
}

async fn publish_batch(events: &[IngestEvent], bus: &dyn EventBus) {
    let payload = serde_json::to_vec(events).unwrap_or_default();
    let _ = bus.publish("iot.events", &payload).await;
}

/// A synthetic message source for load tests / benchmarks. Generates `count` JSON
/// messages alternating between RFID and weight readings.
pub fn synthetic_source(count: usize, seed: u64) -> impl Stream<Item = Vec<u8>> {
    use futures::stream;
    let mut state = seed.wrapping_add(0x1234_5678);
    let mut rng = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    };
    stream::iter((0..count).map(move |i| {
        if rng() % 2 == 0 {
            serde_json::json!({"type":"rfid","reader":format!("G{}", i % 8),"tag":format!("T{}", i)})
        } else {
            serde_json::json!({"type":"weight","sensor":format!("S{}", i % 16),"grams":(i % 5000)})
        }
        .to_string()
        .into_bytes()
    }))
}

#[cfg(feature = "mqtt")]
pub mod mqtt {
    //! Optional MQTT ingress. Bridges an `rumqttc` event loop into the transport-agnostic
    //! [`super::run_pipeline`] by mapping each broker notification to a raw payload.
    //!
    //! This is feature-gated (`--features mqtt`) because it pulls in the MQTT client; the
    //! core pipeline above has no transport dependency.
    use futures::stream::{self, Stream};
    use rumqttc::{Event, Incoming, Packet, QoS, Request};

    /// Adapt an `rumqttc` event loop into a stream of raw payload bytes.
    ///
    /// Only `Publish` packets are forwarded; everything else (ack, suback, ping) is
    /// ignored. The returned stream is finite: it ends when the event loop ends.
    pub fn mqtt_source(mut event_loop: rumqttc::EventLoop) -> impl Stream<Item = Vec<u8>> + '_ {
        stream::unfold(event_loop, |mut el| async move {
            loop {
                match el.poll().await {
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        // `p.payload` is `bytes::Bytes`; copy out so the stream owns it.
                        let bytes: Vec<u8> = p.payload.to_vec();
                        return Some((bytes, el));
                    }
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
        })
    }

    /// Helper to build a request to publish a payload (used by tests/producers).
    pub fn publish_request(topic: String, payload: Vec<u8>) -> Request {
        Request::Publish(rumqttc::Publish::new(topic, QoS::AtLeastOnce, payload))
    }

    // Re-export to keep the `Incoming` name available for callers adapting this.
    #[allow(unused_imports)]
    pub use rumqttc::Incoming as _Incoming;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_bus::memory::InMemoryBus;

    #[tokio::test]
    async fn decodes_rfid_and_weight() {
        let rfid = br#"{"type":"rfid","reader":"G1","tag":"T42"}"#;
        let w = br#"{"type":"weight","sensor":"S7","grams":1234}"#;
        assert_eq!(
            decode(rfid),
            IngestEvent::RfidGate {
                reader: "G1".into(),
                tag: "T42".into()
            }
        );
        assert_eq!(
            decode(w),
            IngestEvent::ConveyorWeight {
                sensor: "S7".into(),
                grams: 1234
            }
        );
        assert_eq!(
            decode(b"not json"),
            IngestEvent::Unparsed(b"not json".to_vec())
        );
    }

    #[tokio::test]
    async fn pipeline_decodes_and_publishes() {
        let bus = InMemoryBus::new();
        let source = synthetic_source(1_000, 1);
        let stats = run_pipeline(source, &bus, 100).await;
        assert_eq!(stats.decoded, 1_000);
        assert_eq!(stats.unparsed, 0);
        assert_eq!(stats.published, 1_000);
    }

    #[tokio::test]
    async fn unparsed_messages_are_counted_not_dropped() {
        let bus = InMemoryBus::new();
        let msgs = vec![
            br#"{"type":"rfid","reader":"G1","tag":"A"}"#.to_vec(),
            b"garbage".to_vec(),
        ];
        let source = futures::stream::iter(msgs);
        let stats = run_pipeline(source, &bus, 10).await;
        assert_eq!(stats.decoded, 1);
        assert_eq!(stats.unparsed, 1);
        assert_eq!(stats.published, 2);
    }

    /// Load test: ingest tens of thousands of messages and assert we sustain a
    /// high message rate (thousands/sec). Ignored in normal CI; run with
    /// `cargo test -p wms --release -- --ignored benchmark_iot_ingestion`.
    #[test]
    #[ignore]
    fn benchmark_iot_ingestion() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let bus = InMemoryBus::new();
            let count = 50_000;
            let source = synthetic_source(count, 99);
            let stats = run_pipeline(source, &bus, 256).await;
            let per_sec = count as f64 / stats.elapsed.as_secs_f64();
            println!(
                "ingested {count} IoT messages in {:?} (~{per_sec:.0} msg/sec)",
                stats.elapsed
            );
            assert_eq!(stats.decoded, count as u64);
            assert!(
                per_sec > 1_000.0,
                "expected >1000 msg/sec, got {per_sec:.0}"
            );
        });
    }
}
