# Security policy

## Supported versions

Only the latest released `0.x` version receives security fixes.

## Reporting a vulnerability

Please do not open a public issue for security problems.

Report privately through GitHub's
[private vulnerability reporting](https://github.com/tpt-solutions/tpt-erp-rust/security/advisories/new)
for this repository. Include a description, steps to reproduce, and the affected crate
or version. We aim to acknowledge reports within 5 business days.

## Scope

In scope: the framework crates under `crates/`, the reference servers under `examples/`,
the Dockerfile, and the Helm chart under `deploy/`.

Out of scope: the dev-only defaults documented in `.env.example` and `docker-compose.yml`
(for example `change-me-in-local-dev-only`). A real deployment must set its own
`TPT_JWT_SECRET`.
