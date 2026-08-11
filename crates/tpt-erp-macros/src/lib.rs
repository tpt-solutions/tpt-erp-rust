//! Procedural macros for TPT ERP.
//!
//! - [`StateMachine`] — derive a transition-checked state enum.
//! - [`TptEntity`] — map a struct to a SQL table, add validation + audit + a query
//!   filter. Backed by the runtime traits in `tpt-erp-entity`.
//! - [`TptApi`] — generate an Axum CRUD router (pagination, filtering, RBAC) for a
//!   [`TptEntity`].

mod tpt_api;
mod tpt_entity_impl;
mod util;

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    DeriveInput, Ident, Result, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

/// Helper attribute parsed from `#[state_machine(transitions(From => To, ...))]`.
struct StateMachineAttr {
    transitions: Vec<(Ident, Ident)>,
}

impl Parse for StateMachineAttr {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let kw: Ident = input.parse()?;
        if kw != "transitions" {
            return Err(syn::Error::new(kw.span(), "expected `transitions(..)`"));
        }
        let inner;
        syn::parenthesized!(inner in input);
        let mut transitions = Vec::new();
        while !inner.is_empty() {
            let from: Ident = inner.parse()?;
            let _: Token![=>] = inner.parse()?;
            let to: Ident = inner.parse()?;
            transitions.push((from, to));
            if inner.peek(Token![,]) {
                inner.parse::<Token![,]>()?;
            }
        }
        Ok(StateMachineAttr { transitions })
    }
}

/// Derive a transition-checked state machine for a unit enum.
///
/// ```ignore
/// #[derive(StateMachine)]
/// #[state_machine(transitions(
///     Draft => Confirmed,
///     Confirmed => Shipped,
/// ))]
/// enum Order { Draft, Confirmed, Shipped }
/// ```
///
/// Generates:
/// - `fn can_transition(&self, to: Self) -> bool`
/// - `fn transition(self, to: Self) -> Result<Self, <Name>TransitionError>`
/// - a `<Name>TransitionError` type carrying `from`/`to`.
#[proc_macro_derive(StateMachine, attributes(state_machine))]
pub fn derive_state_machine(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let enum_name = &input.ident;
    let attr = match input
        .attrs
        .iter()
        .find(|a| a.path().is_ident("state_machine"))
    {
        Some(a) => match a.parse_args::<StateMachineAttr>() {
            Ok(a) => a,
            Err(e) => return e.to_compile_error().into(),
        },
        None => {
            return syn::Error::new_spanned(
                &input,
                "missing `#[state_machine(transitions(..))]` attribute",
            )
            .to_compile_error()
            .into();
        }
    };

    let variants: Vec<Ident> = match &input.data {
        syn::Data::Enum(e) => e.variants.iter().map(|v| v.ident.clone()).collect(),
        _ => {
            return syn::Error::new_spanned(&input, "StateMachine can only be derived for enums")
                .to_compile_error()
                .into();
        }
    };

    let error_name = Ident::new(&format!("{enum_name}TransitionError"), enum_name.span());
    let transition_arms = attr.transitions.iter().map(|(from, to)| {
        quote! { (#enum_name::#from, #enum_name::#to) }
    });

    let _ = &variants;

    let expanded = quote! {
        impl #enum_name {
            /// Whether a transition from `self` to `to` is allowed by the state graph.
            pub fn can_transition(&self, to: #enum_name) -> bool {
                matches!((self, to), #(#transition_arms)|* )
            }

            /// Attempt to transition to `to`, returning an error if the transition is
            /// not allowed by the state graph.
            pub fn transition(self, to: #enum_name) -> ::core::result::Result<#enum_name, #error_name> {
                if self.can_transition(to) {
                    ::core::result::Result::Ok(to)
                } else {
                    ::core::result::Result::Err(#error_name { from: self, to })
                }
            }
        }

        /// Error returned when an invalid state transition is attempted.
        pub struct #error_name {
            /// The state we tried to leave.
            pub from: #enum_name,
            /// The state we tried to enter.
            pub to: #enum_name,
        }

        impl ::core::fmt::Debug for #error_name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#error_name))
                    .field("from", &self.from)
                    .field("to", &self.to)
                    .finish()
            }
        }

        impl ::core::fmt::Display for #error_name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "invalid transition {:?} => {:?}", self.from, self.to)
            }
        }

        impl ::core::error::Error for #error_name {}
    };

    expanded.into()
}

/// Map a struct to a SQL table, adding validation, audit fields, and a query
/// filter. See [`tpt_erp_entity`] for the backing runtime traits and usage.
///
/// Requires the consumer crate to depend on `tpt-erp-entity`, `serde`, and (for DB
/// mapping) `sqlx` (add `#[derive(sqlx::FromRow)]` alongside this derive).
#[proc_macro_derive(TptEntity, attributes(tpt_entity, id, audit, validate))]
pub fn derive_tpt_entity_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match tpt_entity_impl::derive_tpt_entity(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Generate an Axum CRUD router (pagination, filtering, RBAC) for a
/// [`TptEntity`]. Requires the consumer crate to depend on `tpt-erp-entity` and
/// `axum`.
#[proc_macro_derive(TptApi, attributes(tpt_api))]
pub fn derive_tpt_api_entry(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match tpt_api::derive(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
