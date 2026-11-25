# Deployment & Operations

The `deploy/` directory contains a Helm chart that runs the high-throughput ingestion
tier (and, optionally, the six reference servers) behind Kubernetes autoscaling. This
guide covers deploying and operating it.

## Prerequisites

- A Kubernetes cluster (1.29+) with an ingress controller.
- Helm 3.
- Postgres and Redis/Dragonfly available (in-cluster or managed). The chart can deploy
  both — see `deploy/templates`. The event bus runs on Postgres, so it needs no separate
  messaging service.

## Install backing services

```bash
helm dependency update deploy
helm install tpt-erp deploy \
  --namespace tpt-erp --create-namespace \
  --set postgres.enabled=true \
  --set redis.enabled=true
```

This brings up:

- **Postgres** with Row-Level Security (see `tpt-erp-tenant::rls`). Tenant isolation is
  enforced at the database engine, not just in app code.
- The **event bus and background jobs** run on Postgres (`tpt_bus_messages` table, managed by
  `tpt-erp-bus`'s `postgres` feature), so they share Postgres backups and RLS-aware ops.
- **Redis/Dragonfly** for tenant-scoped sessions and CQRS read-model caching.

## Deploy an application server

```bash
helm upgrade --install tpt-erp deploy \
  --namespace tpt-erp \
  --set server.image.repository=ghcr.io/tpt-solutions/tpt-erp-server \
  --set server.image.tag=0.1.0 \
  --set server.replicas=3
```

## Configuration

Key values (see `deploy/values.yaml`):

| Key | Meaning |
|-----|---------|
| `server.replicas` | Desired server replicas (HPA scales beyond this). |
| `ingestion.autoscaling` | HPA min/max replicas for the ingestion tier. |
| `env.RUST_LOG` | Log filter passed to the `tracing` subscriber. |
| `env.TPT_CACHE_URL` | Redis/Dragonfly URL. |

## Observability

- **Logs / tracing:** services call `tpt_erp_observability::init_tracing()`, which reads
  `RUST_LOG`. Forward container stdout to your log stack.
- **Metrics:** each service exposes a Prometheus `/metrics` endpoint (mounted by
  `tpt_erp_observability::metrics_router`). Scrape it with a `ServiceMonitor` or
  `PodMonitor`.

## Health checks

The server exposes `GET /health`. The chart configures `livenessProbe` and
`readinessProbe` against it; do not route traffic to a pod until it is ready.

## Security notes

- RLS keeps tenants isolated at the storage layer. Always set `app.tenant_id` per
  transaction (the `tenant_rls_middleware` does this) and never reuse a connection
  across tenants without re-issuing `SET LOCAL`.
- WASM plugins run in a fuel/memory/epoch-sandboxed `wasmtime` runtime; cap module size
  via `RuntimeConfig::max_module_bytes` and validate plugins against the `plugin` WIT
  world before upload (`tpt plugin validate`).
