//! # wms — reference 3PL / WMS implementation on TPT ERP.
//!
//! This crate is both a marketing demo and a stress-test of the framework. It builds two
//! of the hardest real-time logistics subsystems *without* touching the ledger's core:
//!
//! - [`inventory`] — a real-time, event-sourced inventory engine. Writes append movement
//!   events sharded by bin, so concurrent bin updates never block on a global lock. On-hand
//!   is a CQRS read model rebuilt from the event log, cached per tenant via `tpt-erp-cache`, and
//!   replenishment is emitted as a background job on `tpt-erp-bus`.
//! - [`picking`] — wave & route optimization: picker-path strategies (naive, nearest
//!   neighbor, batch, S-shaped) compared by travel distance.

pub mod ingest;
pub mod inventory;
pub mod picking;
