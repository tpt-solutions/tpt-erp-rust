//! Currency marker types and the [`Currency`] trait.
//!
//! A currency is a zero-sized marker used as the `C` parameter of [`crate::Money`].
//! Because `Money<Usd>` and `Money<Eur>` are distinct types, the compiler refuses to
//! add them together — currency mismatch is a compile error, not a runtime bug.

/// A currency marker. Implementors are typically zero-sized types.
///
/// The associated `code`/`symbol`/`minor_units` exist for display and (de)serialization
/// but are not required for the type-level safety guarantees.
pub trait Currency: 'static + Send + Sync + Copy + core::fmt::Debug {
    /// ISO-4217 alpha code, e.g. `"USD"`.
    const CODE: &'static str;
    /// Display symbol, e.g. `"$"`.
    const SYMBOL: &'static str;
    /// Number of decimal places in the minor unit (USD=2, JPY=0).
    const MINOR_UNITS: u32;
}

macro_rules! currency_marker {
    ($name:ident, $code:literal, $symbol:literal, $minor:literal) => {
        /// Auto-generated currency marker type.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub struct $name;

        impl Currency for $name {
            const CODE: &'static str = $code;
            const SYMBOL: &'static str = $symbol;
            const MINOR_UNITS: u32 = $minor;
        }
    };
}

currency_marker!(Usd, "USD", "$", 2);
currency_marker!(Eur, "EUR", "€", 2);
currency_marker!(Gbp, "GBP", "£", 2);
currency_marker!(Jpy, "JPY", "¥", 0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_minor_units() {
        assert_eq!(Usd::CODE, "USD");
        assert_eq!(Usd::MINOR_UNITS, 2);
        assert_eq!(Jpy::MINOR_UNITS, 0);
        assert_eq!(Eur::SYMBOL, "€");
    }
}
