//! `#[derive(TptEntity)]` — table mapping, validation, audit, and query filter.
//!
//! Implemented against the runtime traits in `tpt-entity`. The generated code
//! references the `tpt_entity` crate, so the consumer must depend on it.
//!
//! ```ignore
//! #[derive(Debug, Clone, TptEntity, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
//! #[tpt_entity(table = "customers")]
//! struct Customer {
//!     #[id]
//!     id: Id<Customer>,
//!     #[validate(required, len(min = 1, max = 200), email)]
//!     email: String,
//!     #[validate(range(min = 0, max = 120))]
//!     age: i32,
//!     #[audit]
//!     created_at: DateTime<Utc>,
//!     #[audit]
//!     updated_at: DateTime<Utc>,
//!     #[audit]
//!     created_by: Option<String>,
//!     #[audit]
//!     updated_by: Option<String>,
//! }
//! ```

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Field, Ident, LitInt, LitStr, Result, Type};

use crate::util::to_snake_case;

/// A single `#[validate(...)]` directive attached to a field.
enum Validator {
    Required,
    Email,
    Len(usize, usize),
    Range(i128, i128),
}

/// Whether a field type is a `String`, an `Option<_>`, or something else.
#[derive(PartialEq, Eq)]
enum Kind {
    String,
    Option,
    Other,
}

fn kind_of(ty: &Type) -> Kind {
    if let Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            return match seg.ident.to_string().as_str() {
                "String" => Kind::String,
                "Option" => Kind::Option,
                _ => Kind::Other,
            };
        }
    }
    Kind::Other
}

/// Parse the validators on one field from its `#[validate(...)]` attributes.
fn field_validators(field: &Field) -> Result<Vec<Validator>> {
    let mut out = Vec::new();
    for attr in &field.attrs {
        if !attr.path().is_ident("validate") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("required") {
                out.push(Validator::Required);
                return Ok(());
            }
            if meta.path.is_ident("email") {
                out.push(Validator::Email);
                return Ok(());
            }
            if meta.path.is_ident("len") {
                let mut min = 0usize;
                let mut max = usize::MAX;
                meta.parse_nested_meta(|inner| {
                    if inner.path.is_ident("min") {
                        let v: LitInt = inner.value()?.parse()?;
                        min = v.base10_parse()?;
                    } else if inner.path.is_ident("max") {
                        let v: LitInt = inner.value()?.parse()?;
                        max = v.base10_parse()?;
                    }
                    Ok(())
                })?;
                out.push(Validator::Len(min, max));
                return Ok(());
            }
            if meta.path.is_ident("range") {
                let mut min = i128::MIN;
                let mut max = i128::MAX;
                meta.parse_nested_meta(|inner| {
                    if inner.path.is_ident("min") {
                        let v: LitInt = inner.value()?.parse()?;
                        min = v.base10_parse()?;
                    } else if inner.path.is_ident("max") {
                        let v: LitInt = inner.value()?.parse()?;
                        max = v.base10_parse()?;
                    }
                    Ok(())
                })?;
                out.push(Validator::Range(min, max));
                return Ok(());
            }
            Err(meta.error("unknown validator; expected required/email/len/range"))
        })?;
    }
    Ok(out)
}

fn is_id(field: &Field) -> bool {
    field.attrs.iter().any(|a| a.path().is_ident("id"))
}

fn is_audit(field: &Field) -> bool {
    field.attrs.iter().any(|a| a.path().is_ident("audit"))
}

/// Build the validation body for one field.
fn validate_stmt(ident: &Ident, ty: &Type, validators: &[Validator]) -> TokenStream {
    let name = ident.to_string();
    let kind = kind_of(ty);
    let mut checks = Vec::new();
    for v in validators {
        match v {
            Validator::Required => match kind {
                Kind::String => checks.push(quote! {
                    if self.#ident.trim().is_empty() {
                        return ::core::result::Result::Err(
                            ::tpt_entity::ValidationError::Required(#name));
                    }
                }),
                Kind::Option => checks.push(quote! {
                    if self.#ident.is_none() {
                        return ::core::result::Result::Err(
                            ::tpt_entity::ValidationError::Required(#name));
                    }
                }),
                Kind::Other => {}
            },
            Validator::Email => {
                if kind == Kind::String {
                    checks.push(quote! {
                        if !self.#ident.contains('@') || !self.#ident.contains('.') {
                            return ::core::result::Result::Err(
                                ::tpt_entity::ValidationError::Email(#name));
                        }
                    });
                }
            }
            Validator::Len(min, max) => {
                if kind == Kind::String {
                    let min = *min;
                    let max = *max;
                    checks.push(quote! {
                        {
                            let len = self.#ident.len();
                            if len < #min || len > #max {
                                return ::core::result::Result::Err(
                                    ::tpt_entity::ValidationError::Length(#name, len, #min, #max));
                            }
                        }
                    });
                }
            }
            Validator::Range(min, max) => {
                let min = *min;
                let max = *max;
                checks.push(quote! {
                    {
                        let v = self.#ident as i128;
                        let in_range = (#min..=#max).contains(&v);
                        if !in_range {
                            return ::core::result::Result::Err(
                                ::tpt_entity::ValidationError::Range(
                                    #name, v.to_string(),
                                    (#min as i128).to_string(), (#max as i128).to_string()));
                        }
                    }
                });
            }
        }
    }
    quote! { #(#checks)* }
}

pub(crate) fn derive(input: DeriveInput) -> Result<TokenStream> {
    let entity = &input.ident;
    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "TptEntity can only be derived for structs",
            ))
        }
    };
    let named = match fields {
        syn::Fields::Named(n) => &n.named,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "TptEntity requires named fields",
            ))
        }
    };

    // --- container attributes -------------------------------------------
    let mut table = None;
    let mut id_column = "id".to_string();
    for attr in &input.attrs {
        if attr.path().is_ident("tpt_entity") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("table") {
                    let v: LitStr = meta.value()?.parse()?;
                    table = Some(v.value());
                    Ok(())
                } else if meta.path.is_ident("id_column") {
                    let v: LitStr = meta.value()?.parse()?;
                    id_column = v.value();
                    Ok(())
                } else {
                    Err(meta.error("unknown tpt_entity key; expected table/id_column"))
                }
            })?;
        }
    }
    let table = table.unwrap_or_else(|| to_snake_case(&entity.to_string()) + "s");

    // --- locate id + audit fields, build filter + validation -------------
    let mut id_field = None;
    let mut audit = std::collections::HashMap::new();
    let mut filter_fields = Vec::new();
    let mut validate_body = Vec::new();
    let mut apply_body = Vec::new();

    for f in named {
        let ident = f.ident.as_ref().unwrap();
        let ty = &f.ty;
        let is_id = is_id(f);
        let is_audit = is_audit(f);

        if is_id {
            id_field = Some((ident.clone(), ty.clone()));
        }
        if is_audit {
            audit.insert(ident.to_string(), ident.clone());
        }

        // validation
        let validators = field_validators(f)?;
        if !validators.is_empty() {
            validate_body.push(validate_stmt(ident, ty, &validators));
        }

        // filter: every non-id, non-audit field becomes an optional filter column
        if !is_id && !is_audit {
            filter_fields.push((ident.clone(), ty.clone()));
            apply_body.push(quote! {
                if let ::core::option::Option::Some(v) = &self.#ident {
                    if entity.#ident != *v { return false; }
                }
            });
        }
    }

    let (id_ident, id_ty) = id_field.ok_or_else(|| {
        syn::Error::new_spanned(&input, "TptEntity requires exactly one #[id] field")
    })?;

    let filter_name = Ident::new(&format!("{entity}Filter"), entity.span());
    // Pagination is folded into the filter so a single top-level `Query`
    // (axum uses serde_html_form, which mishandles `#[serde(flatten)]`) can
    // carry both paging and filtering without flattening.
    let mut filter_decls: Vec<TokenStream> = vec![
        quote! { pub page: ::core::option::Option<u32> },
        quote! { pub per_page: ::core::option::Option<u32> },
    ];
    filter_decls.extend(filter_fields.iter().map(|(ident, ty)| {
        quote! { pub #ident: ::core::option::Option<#ty> }
    }));

    let audit_created_at = audit.get("created_at");
    let audit_updated_at = audit.get("updated_at");
    let audit_created_by = audit.get("created_by");
    let audit_updated_by = audit.get("updated_by");

    let auditable_impl = if let (Some(ca), Some(ua), Some(cb), Some(ub)) = (
        audit_created_at,
        audit_updated_at,
        audit_created_by,
        audit_updated_by,
    ) {
        quote! {
            impl ::tpt_entity::Auditable for #entity {
                fn audit_fields(&self) -> ::core::option::Option<::tpt_entity::AuditFields> {
                    ::core::option::Option::Some(::tpt_entity::AuditFields {
                        created_at: self.#ca,
                        updated_at: self.#ua,
                        created_by: self.#cb.clone(),
                        updated_by: self.#ub.clone(),
                    })
                }
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl ::tpt_entity::EntityTable for #entity {
            fn table_name() -> &'static str { #table }
            fn id_column() -> &'static str { #id_column }
            type Id = #id_ty;
            type Filter = #filter_name;
            fn id(&self) -> Self::Id { self.#id_ident }
        }

        impl ::tpt_entity::Validatable for #entity {
            #[allow(clippy::manual_range_contains)]
            fn validate(&self) -> ::core::result::Result<(), ::tpt_entity::ValidationError> {
                #(#validate_body)*
                ::core::result::Result::Ok(())
            }
        }

        #auditable_impl

        /// Auto-generated all-optional query filter for `#entity`.
        #[derive(Debug, Clone, Default, ::serde::Serialize, ::serde::Deserialize)]
        pub struct #filter_name {
            #(#filter_decls),*
        }

        impl ::tpt_entity::Filter for #filter_name {}

        impl ::tpt_entity::ApplyFilter<#entity> for #filter_name {
            fn matches(&self, entity: &#entity) -> bool {
                #(#apply_body)*
                true
            }
        }
    };

    Ok(expanded)
}

/// Entry point registered in `lib.rs`.
pub(crate) fn derive_tpt_entity(input: DeriveInput) -> Result<TokenStream> {
    derive(input)
}
