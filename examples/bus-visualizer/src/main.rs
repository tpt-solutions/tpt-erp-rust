//! Live event-bus visualizer for TPT ERP.
//!
//! Subscribes to the `tpt-erp-bus` and streams every event to a browser over Server-Sent
//! Events, giving a real-time view of the cross-vertical flow OMS → WMS → TMS → GL. It is
//! both a sales demo and a debugging tool for the event-sourced architecture: point it at a
//! NATS JetStream backend (`TPT_BUS_URL`) and watch production events flow, or run it
//! locally and let it drive the reference `flow` so the page never goes quiet.
//!
//! ```bash
//! cargo run -p bus-visualizer
//! # open http://localhost:3000
//! ```

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::routing::get;
use futures::StreamExt;
use serde_json::json;
use tpt_erp_bus::EventBus;
use tpt_erp_bus::memory::InMemoryBus;
use tpt_erp_observability::init_tracing;
use tpt_erp_primitives::{Money, Usd};

/// Shared application state: the bus the visualizer subscribes to.
#[derive(Clone)]
struct AppState {
    bus: Arc<dyn EventBus>,
}

/// Static HTML page: an EventSource that renders incoming bus events as a live log.
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>TPT ERP — Event Bus Visualizer</title>
  <style>
    body { font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; background:#0f1115; color:#e6e6e6; margin:0; padding:1rem; }
    h1 { font-size: 1rem; font-weight: 600; }
    #sub { color:#8b949e; margin-bottom: 1rem; }
    #log { list-style:none; margin:0; padding:0; }
    #log li { padding:.35rem .5rem; border-left:3px solid #2ea043; background:#161b22; margin-bottom:.35rem; border-radius:3px; white-space:pre-wrap; word-break:break-word; }
    .subject { color:#58a6ff; }
    .time { color:#8b949e; }
  </style>
</head>
<body>
  <h1>TPT ERP — Event Bus Visualizer</h1>
  <div id="sub">Live events from <code>tpt-erp-bus</code> (OMS → WMS → TMS → GL)</div>
  <ul id="log"></ul>
  <script>
    const log = document.getElementById("log");
    const es = new EventSource("/stream");
    es.onmessage = (e) => {
      const ev = JSON.parse(e.data);
      const li = document.createElement("li");
      li.innerHTML = `<span class="time">${new Date(ev.ts).toLocaleTimeString()}</span> ` +
                     `<span class="subject">${ev.subject}</span>\n` + ev.payload;
      log.prepend(li);
      while (log.childElementCount > 200) log.removeChild(log.lastChild);
    };
  </script>
</body>
</html>"#;

/// `GET /` — the visualizer page.
async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// `GET /stream` — Server-Sent Events of every bus message (wildcard `>` subscription).
async fn stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    let sub = state.bus.subscribe(">").await.expect("subscribe to bus");

    let s = futures::stream::unfold(sub, |mut sub| async move {
        match sub.next().await {
            Some(msg) => {
                let payload = String::from_utf8(msg.payload.clone())
                    .unwrap_or_else(|_| serde_json::to_string(&msg.payload).unwrap_or_default());
                // Best-effort pretty JSON; fall back to the raw string.
                let pretty = serde_json::from_str::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .unwrap_or(payload);
                let data = json!({
                    "subject": msg.subject,
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "payload": pretty,
                })
                .to_string();
                Some((Ok(SseEvent::default().event("bus-event").data(data)), sub))
            }
            None => None,
        }
    });

    Sse::new(s).keep_alive(axum::response::sse::KeepAlive::default())
}

#[tokio::main]
async fn main() {
    init_tracing();

    let bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new());
    let tenant = flow::demo_tenant();
    // $12.50 per unit, order 4 units (matches the flow reference demo).
    let unit_cost =
        Money::<Usd>::from_major(12) + Money::<Usd>::new(rust_decimal::Decimal::new(50, 2));

    // Drive the reference cross-vertical flow on a loop so the visualizer keeps streaming.
    // Each run gets its own bus; a bridge republishes its events onto the shared bus the
    // SSE view subscribes to, so the page stays live without the flow's per-run subscribers
    // leaking into the shared bus.
    let shared_bus = bus.clone();
    tokio::spawn(async move {
        loop {
            let run_bus: Arc<dyn EventBus> = Arc::new(InMemoryBus::new());
            let bridge_target = shared_bus.clone();
            if let Ok(mut bridge) = run_bus.subscribe(">").await {
                tokio::spawn(async move {
                    while let Some(m) = bridge.next().await {
                        let _ = bridge_target.publish(&m.subject, &m.payload).await;
                    }
                });
            }
            if let Err(e) = flow::run_flow_on(tenant, 4, unit_cost, run_bus.clone()).await {
                tracing::warn!(error = %e, "flow run failed");
            }
            tokio::time::sleep(Duration::from_secs(8)).await;
        }
    });

    let state = Arc::new(AppState { bus });
    let app = Router::new()
        .route("/", get(index))
        .route("/stream", get(stream))
        .with_state(state);

    let addr = std::env::var("TPT_BIND")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string())
        .parse::<std::net::SocketAddr>()
        .expect("invalid TPT_BIND address");

    tracing::info!(%addr, "bus visualizer listening");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}
