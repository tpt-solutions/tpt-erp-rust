# tpt-erp-tenant

> Guarantees tenant isolation at the **database engine** level — not just in
> application code.

`tpt-erp-tenant` resolves which tenant a request belongs to and produces the
Postgres machinery that scopes every query to it. Because isolation is enforced
by Row-Level Security, a mistakenly cross-tenant query is rejected by Postgres
itself, not by a missed `WHERE` clause.

## Modules

| Module | Responsibility |
|--------|----------------|
| [`identification`](src/identification.rs) | Extract the active tenant from subdomain / header / JWT claim; map a slug to a stable `TenantId`. |
| [`rls`](src/rls.rs) | RLS policy templates and the `SET LOCAL` command that scopes a transaction to a tenant. |
| `web` *(feature `axum`)* | Axum extractor + resolver middleware wiring the above to a request. |

## Tenant identification

A request may carry its tenant in several places. The pure helpers
`from_subdomain`, `from_header`, and `from_jwt_claims` extract a [`TenantSlug`]
from each:

- `acme.example.com` → slug `acme` (leftmost DNS label)
- `X-Tenant-Id: globex` → slug `globex`
- JWT claim `"tenant": "globex"` → slug `globex`

A slug is validated (non-empty, `[A-Za-z0-9_-]`) and mapped deterministically to
a stable [`TenantId`] (UUID v5) via `TenantSlug::to_id()`, so tenant-scoped
storage and isolation are reproducible without a database round-trip.

## Postgres Row-Level Security

The `rls` module builds the SQL that makes isolation real:

```rust
use tpt_erp_tenant::{set_tenant_command, rls_policy, enable_rls, TENANT_GUC};

// Per transaction, the middleware runs:
let cmd = set_tenant_command(&tenant_id); // SET LOCAL app.tenant_id = '<uuid>'

// Migration-time policy (one per tenant-scoped table):
let policy = rls_policy("orders", "tenant_id", "orders_tenant");
// CREATE POLICY orders_tenant ON orders FOR ALL
//   USING (tenant_id = current_setting('app.tenant_id')::uuid)
let _ = enable_rls("orders");
```

Every relevant table gets a policy comparing its tenant column to the
`app.tenant_id` session setting. The value is always an alphanumeric UUID, so
there is no SQL-injection surface.

## Axum integration *(feature `axum`)*

```toml
tpt-erp-tenant = { features = ["axum"] }
```

- `TenantContext` is an extractor: declare it as a handler parameter and it is
  resolved from the `Host` subdomain or `X-Tenant-Id` header.
- `tenant_context_middleware` pre-resolves the tenant and stashes it in request
  extensions (and computes the `SET LOCAL` command a pooled connection would
  execute per transaction).

A missing/invalid tenant is rejected with `400 BAD REQUEST`.

## Status

Early development (0.1.0). Identification and RLS primitives are pure and fully
unit-tested; the Axum middleware is enabled via the `axum` feature. APIs may
change between releases.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/Apache-2.0) at your option.
