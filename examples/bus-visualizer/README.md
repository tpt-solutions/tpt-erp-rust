# bus-visualizer

> Live event-bus visualizer: streams cross-vertical events to a browser over Server-Sent Events.

`bus-visualizer` subscribes to `tpt-erp-bus` and streams every event to the browser,
giving a real-time view of the OMS → WMS → TMS → GL flow. It works as a sales demo and
as a debugging tool for the event-sourced architecture.

## Run

Against a NATS JetStream backend, point it at your broker:

```bash
TPT_BUS_URL=nats://localhost:4222 cargo run -p bus-visualizer
```

Or run it locally. It drives the reference `flow` crate so the page keeps receiving
events.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
