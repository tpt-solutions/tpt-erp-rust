# Changelog

All notable changes to this crate are documented in this file. This project adheres to [Semantic Versioning](https://semver.org).

## [0.1.0] - 2026-08-03

- Initial workspace scaffold and crate extraction.
- Event-processing and background-job bus for TPT ERP. Decision: PostgreSQL-backed bus (feature `postgres`), so durable events share the ledger's database and add no messaging service. In-memory reference implementation included.
- Replaced the earlier NATS JetStream backend (`nats` feature) with the Postgres backend. See the root CHANGELOG for the breaking change.