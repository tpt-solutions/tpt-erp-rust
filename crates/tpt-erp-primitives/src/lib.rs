//! # tpt-erp-primitives
//!
//! Zero-cost domain-modeling abstractions for TPT ERP. These types push invalid
//! states to the type level so they are caught at compile time.
//!
//! - [`Money`] — currency-tagged decimal money that forbids cross-currency math.
//! - [`Id`] — strongly typed identifiers that forbid cross-entity mixups.
//! - [`StateMachine`] — derive macro (re-exported from `tpt-erp-macros`) that generates
//!   transition-checked state enums.

pub mod currency;
pub mod id;
pub mod money;

pub use currency::{Currency, Eur, Gbp, Jpy, Usd};
pub use id::{Entity, Id, IntId};
pub use money::Money;

/// Re-export of the `StateMachine` derive macro so downstream crates only depend on
/// `tpt-erp-primitives` to get domain primitives and the state-machine derive.
pub use tpt_erp_macros::StateMachine;

/// Common rounding behaviour for monetary calculations.
pub use rust_decimal::RoundingStrategy;
