# tpt-erp-cache

> Session management and CQRS read-model caching for TPT ERP.

`tpt-erp-cache` provides tenant-scoped, TTL-aware caching so the system does not
rebuild expensive materialized views (via `tpt-erp-ledger` projections) or
re-authenticate sessions on every request. Application code depends only on two
object-safe trait contracts; the backend is selected by feature flag.

## Contracts

### `SessionStore`

Tenant-scoped web sessions with absolute and sliding TTLs.

```rust
use tpt_erp_cache::{SessionStore, InMemoryCache};
use tpt_erp_tenant::TenantSlug;
use std::time::Duration;

let cache = InMemoryCache::new();
let tenant = TenantSlug("acme".into()).to_id();
let session = cache.create(tenant, serde_json::json!({"role":"admin"}),
                           Some(Duration::from_secs(3600))).await?;
assert!(cache.get(&session.id).await?.is_some());
cache.touch(&session.id).await?; // slide TTL forward
```

Implementations enforce **tenant isolation**: a session for one tenant is never
readable by another, and `delete_for_tenant` removes only that tenant's sessions.

### `ReadModelCache`

The CQRS read-model cache. Keys are namespaced by `(tenant, model, key)`, so
cross-tenant leakage is impossible by construction.

```rust
use tpt_erp_cache::{ReadModelCache, InMemoryCache};

let cache = InMemoryCache::new();
cache.put(&tenant, "balances", "acc-1", serde_json::json!({"n":1}), None).await?;
let v = cache.get(&tenant, "balances", "acc-1").await?;
cache.invalidate_model(&tenant, "balances").await?;
```

## Backends

| Backend | Feature | Notes |
|---------|---------|-------|
| `InMemoryCache` ([`memory`](src/memory.rs)) | default (always on) | Process-local; tests and single-node local runs. |
| `RedisSessionStore` / `RedisReadModelCache` ([`redis_impl`](src/redis_impl.rs)) | `redis` | Redis / Dragonfly. Keys namespaced (`tpt:sess:{id}`, `tpt:rm:{tid}:{model}:{k}`) so tenant isolation holds at the storage layer too. |

```toml
tpt-erp-cache = { features = ["redis"] }
```

```rust
let sessions = tpt_erp_cache::RedisSessionStore::connect("redis://localhost").await?;
let models   = tpt_erp_cache::RedisReadModelCache::connect("redis://localhost").await?;
```

## Status

Early development (0.1.0). The in-memory implementation is fully tested; the
Redis/Dragonfly backend is feature-gated. APIs may change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
