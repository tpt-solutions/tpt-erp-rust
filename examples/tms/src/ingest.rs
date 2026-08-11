//! GPS telemetry ingestion pipeline for the TMS.
//!
//! Vehicles stream position frames. This module turns that firehose into a
//! back-pressured, batched stream of domain events published onto the `tpt-erp-bus`
//! event backbone (so geofencing and dispatch react via CQRS projections).
//!
//! The pipeline is **transport-agnostic**: a source is any `Stream<Item = Vec<u8>>`
//! (MQTT, raw TCP, a file, or a synthetic generator for load tests). The decode/transform/
//! publish stages are identical regardless of where bytes came from.

use futures::Stream;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use std::time::{Duration, Instant};
use tpt_erp_bus::EventBus;

use crate::geo::LatLng;

/// A decoded GPS event from a vehicle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpsEvent {
    /// Vehicle identifier.
    pub vehicle: String,
    /// Position sample.
    pub pos: LatLng,
    /// Ground speed in km/h.
    pub speed: f64,
    /// Timestamp (epoch milliseconds).
    pub ts: i64,
}

#[derive(Debug, Deserialize)]
struct WireGps {
    vehicle: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
    speed: Option<f64>,
    ts: Option<i64>,
}

/// Decode a single raw GPS payload into a domain [`GpsEvent`]. Wire format is one JSON
/// object per message: `{"vehicle":"V1","lat":40.7,"lng":-74.0,"speed":62,"ts":1700000000}`.
/// A malformed frame yields `None` (and is counted, never silently dropped downstream).
pub fn decode(payload: &[u8]) -> Option<GpsEvent> {
    let w: WireGps = serde_json::from_slice(payload).ok()?;
    Some(GpsEvent {
        vehicle: w.vehicle.unwrap_or_default(),
        pos: LatLng::new(w.lat?, w.lng?),
        speed: w.speed.unwrap_or(0.0),
        ts: w.ts.unwrap_or(0),
    })
}

/// Throughput/correctness counters produced by [`run_pipeline`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestStats {
    /// Number of frames successfully decoded into a domain event.
    pub decoded: u64,
    /// Number of frames that failed to parse.
    pub unparsed: u64,
    /// Number of events published to the bus (after batching).
    pub published: u64,
    /// Wall-clock time the pipeline took.
    pub elapsed: Duration,
}

/// Run the ingestion pipeline to completion.
///
/// * `source` yields raw payloads (one per message).
/// * `bus` receives batched events on the `gps.telemetry` subject.
/// * `batch` is the max events per bus publish (batches cut latency vs. message count).
///
/// Back-pressure is applied naturally: `await`ing the bus throttles the decode stage
/// instead of unbounded buffering, so a slow consumer cannot OOM the ingester.
pub async fn run_pipeline<S>(mut source: S, bus: &dyn EventBus, batch: usize) -> IngestStats
where
    S: Stream<Item = Vec<u8>> + Unpin + Send,
{
    let mut stats = IngestStats::default();
    let start = Instant::now();
    let mut buf: Vec<GpsEvent> = Vec::with_capacity(batch.max(1));

    while let Some(msg) = source.next().await {
        match decode(&msg) {
            Some(ev) => {
                stats.decoded += 1;
                buf.push(ev);
            }
            None => stats.unparsed += 1,
        }
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

async fn publish_batch(events: &[GpsEvent], bus: &dyn EventBus) {
    let payload = serde_json::to_vec(events).unwrap_or_default();
    let _ = bus.publish("gps.telemetry", &payload).await;
}

/// A synthetic message source for load tests / benchmarks. Generates `count` JSON GPS
/// frames along a rough circle around a depot.
pub fn synthetic_source(count: usize, _seed: u64) -> impl Stream<Item = Vec<u8>> {
    use futures::stream;
    let depot = LatLng::new(40.0, -73.0);
    stream::iter((0..count).map(move |i| {
        let a = (i as f64) * 0.01;
        let pos = LatLng::new(depot.lat + a.sin() * 0.1, depot.lng + a.cos() * 0.1);
        serde_json::json!({
            "vehicle": format!("V{}", i % 64),
            "lat": pos.lat,
            "lng": pos.lng,
            "speed": (i % 90),
            "ts": 1_700_000_000 + i,
        })
        .to_string()
        .into_bytes()
    }))
}

#[cfg(feature = "mqtt")]
pub mod mqtt {
    //! Optional MQTT ingress. Bridges an `rumqttc` event loop into the transport-agnostic
    //! [`super::run_pipeline`] by mapping each broker notification to a raw payload.
    //!
    //! Feature-gated (`--features mqtt`) because it pulls in the MQTT client; the core
    //! pipeline above has no transport dependency.
    use futures::stream::{self, Stream};
    use rumqttc::{Event, Incoming, Packet, QoS, Request};

    /// Adapt an `rumqttc` event loop into a stream of raw payload bytes. Only `Publish`
    /// packets are forwarded; everything else (ack, suback, ping) is ignored.
    pub fn mqtt_source(mut event_loop: rumqttc::EventLoop) -> impl Stream<Item = Vec<u8>> + '_ {
        stream::unfold(event_loop, |mut el| async move {
            loop {
                match el.poll().await {
                    Ok(Event::Incoming(Packet::Publish(p))) => {
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

    #[allow(unused_imports)]
    pub use rumqttc::Incoming as _Incoming;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_erp_bus::memory::InMemoryBus;

    #[test]
    fn decodes_gps_frame() {
        let frame = br#"{"vehicle":"V1","lat":40.7,"lng":-74.0,"speed":62,"ts":1700000000}"#;
        let ev = decode(frame).unwrap();
        assert_eq!(ev.vehicle, "V1");
        assert_eq!(ev.pos, LatLng::new(40.7, -74.0));
        assert_eq!(ev.speed, 62.0);
        assert_eq!(decode(b"garbage"), None);
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

    /// Load test: ingest tens of thousands of GPS frames and assert a high message rate.
    /// Ignored in normal CI; run with `cargo test -p tms --release -- --ignored`.
    #[test]
    #[ignore]
    fn benchmark_gps_ingestion() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let bus = InMemoryBus::new();
            let count = 50_000;
            let source = synthetic_source(count, 99);
            let stats = run_pipeline(source, &bus, 256).await;
            let per_sec = count as f64 / stats.elapsed.as_secs_f64();
            println!(
                "ingested {count} GPS frames in {:?} (~{per_sec:.0} msg/sec)",
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
