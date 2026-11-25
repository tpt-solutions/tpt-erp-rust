# flow

> Cross-vertical reference flow: an OMS order triggers a WMS pick, a TMS dispatch, and a GL posting.

`flow` proves the framework's core promise: verticals are decoupled and talk only through
the event bus. A single commerce event fans out across four reference implementations
without any of them calling another directly.

```text
oms.order.created  ──▶  WMS pick          ──▶  wms.pick.done
                   ──▶  TMS dispatch      ──▶  tms.dispatch.done
                   ──▶  GL posting        ──▶  gl.posting.done
```

## Test

```bash
cargo test -p flow
```

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
