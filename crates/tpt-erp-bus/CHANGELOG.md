# Changelog

All notable changes to this crate are documented in this file. This project adheres to [Semantic Versioning](https://semver.org).

## [0.1.0] - 2026-08-03

- Initial workspace scaffold and crate extraction.
- Event-processing and background-job bus for TPT ERP. Decision: NATS JetStream (Rust-native, durable, single-binary). In-memory reference implementation included.