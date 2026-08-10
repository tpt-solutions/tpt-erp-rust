//! # mes — reference Manufacturing / MES implementation on TPT ERP.
//!
//! A stress-test and showcase of the framework for discrete manufacturing:
//!
//! - [`mrp`] — a parallel Material Requirements Planning engine. A Bill-of-Materials tree
//!   is exploded with [rayon](https://docs.rs/rayon), computing gross/net requirements,
//!   shortages, and the critical-path lead time for BOMs of thousands of parts.
//! - [`wip`] — a shop-floor Work-In-Process state machine derived with `tpt-erp-primitives`
//!   `StateMachine`, enforcing legal manufacturing transitions and prerequisite checks.

pub mod mrp;
pub mod oee;
pub mod telemetry;
pub mod wip;
