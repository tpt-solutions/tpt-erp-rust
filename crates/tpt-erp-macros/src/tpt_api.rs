//! `#[derive(TptApi)]` — generate an Axum CRUD router for a [`TptEntity`].
//!
//! ```ignore
//! #[derive(TptApi)]
//! #[tpt_api(entity = Customer, path = "/customers")]
//! struct CustomerApi;
//!
//! let repo = Arc::new(InMemoryRepository::<Customer>::new());
//! let app = CustomerApi::router::<_, AllowAll>(repo);
//! ```
//!
//! The generated router wires `GET /`, `GET /:id`, `POST /`, `PUT /:id`,
//! `DELETE /:id` to a [`Repository`](tpt_erp_entity::Repository), honouring
//! pagination + filtering and invoking an [`AuthPolicy`](tpt_erp_entity::AuthPolicy)
//! before each operation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Ident, LitStr, Result, Type};

use crate::util::to_snake_case;

pub(crate) fn derive(input: DeriveInput) -> Result<TokenStream> {
    let api = &input.ident;
    // TptApi must be derived on a unit struct (it carries no data of its own).
    if !matches!(input.data, Data::Struct(ref s) if matches!(s.fields, syn::Fields::Unit)) {
        return Err(syn::Error::new_spanned(
            &input,
            "TptApi must be derived on a unit struct, e.g. `struct CustomerApi;`",
        ));
    }

    let mut entity_ty: Option<Type> = None;
    let mut path: Option<String> = None;
    let mut auth_ty: Type = syn::parse_quote!(::tpt_erp_entity::AllowAll);

    for attr in &input.attrs {
        if !attr.path().is_ident("tpt_api") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("entity") {
                entity_ty = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("path") {
                let v: LitStr = meta.value()?.parse()?;
                path = Some(v.value());
                Ok(())
            } else if meta.path.is_ident("auth") {
                auth_ty = meta.value()?.parse()?;
                Ok(())
            } else {
                Err(meta.error("unknown tpt_api key; expected entity/path/auth"))
            }
        })?;
    }

    let entity_ty = entity_ty
        .ok_or_else(|| syn::Error::new_spanned(&input, "#[tpt_api(entity = ...)] is required"))?;
    let entity_ident = match &entity_ty {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .ok_or_else(|| syn::Error::new_spanned(&entity_ty, "invalid entity type"))?
            .ident
            .clone(),
        _ => {
            return Err(syn::Error::new_spanned(
                &entity_ty,
                "entity must be a path type",
            ));
        }
    };

    let path = path.unwrap_or_else(|| format!("/{}", to_snake_case(&entity_ident.to_string())));
    let path_id = format!("{path}/{{id}}");

    let state_name = Ident::new(&format!("{api}State"), api.span());
    let error_name = Ident::new(&format!("{api}Error"), api.span());
    let list = Ident::new(&format!("{api}_list",), api.span());
    let get_one = Ident::new(&format!("{api}_get_one"), api.span());
    let create = Ident::new(&format!("{api}_create"), api.span());
    let replace = Ident::new(&format!("{api}_replace"), api.span());
    let delete = Ident::new(&format!("{api}_delete"), api.span());
    let ensure_principal = Ident::new(&format!("{api}_ensure_principal"), api.span());

    let expanded = quote! {
        /// Generated state shared by the CRUD handlers.
        struct #state_name<R: ::tpt_erp_entity::Repository<#entity_ty> + 'static, A: ::tpt_erp_entity::AuthPolicy> {
            repo: ::std::sync::Arc<R>,
            _auth: ::core::marker::PhantomData<A>,
        }

        impl<R: ::tpt_erp_entity::Repository<#entity_ty> + 'static, A: ::tpt_erp_entity::AuthPolicy> ::core::clone::Clone
            for #state_name<R, A>
        {
            fn clone(&self) -> Self {
                Self { repo: self.repo.clone(), _auth: ::core::marker::PhantomData }
            }
        }

        /// Generated error type returned by the CRUD handlers.
        #[derive(Debug)]
        enum #error_name {
            Forbidden,
            NotFound,
            BadRequest(String),
            Conflict(String),
            Internal(String),
        }

        impl ::core::fmt::Display for #error_name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "{:?}", self)
            }
        }

        impl ::core::error::Error for #error_name {}

        impl ::core::convert::From<::tpt_erp_entity::RepositoryError> for #error_name {
            fn from(e: ::tpt_erp_entity::RepositoryError) -> Self {
                match e {
                    ::tpt_erp_entity::RepositoryError::NotFound => #error_name::NotFound,
                    ::tpt_erp_entity::RepositoryError::Validation(v) => #error_name::BadRequest(v.to_string()),
                    ::tpt_erp_entity::RepositoryError::Conflict(m) => #error_name::Conflict(m),
                    ::tpt_erp_entity::RepositoryError::Backend(m) => #error_name::Internal(m),
                }
            }
        }

        impl ::axum::response::IntoResponse for #error_name {
            fn into_response(self) -> ::axum::response::Response {
                let (status, msg): (::axum::http::StatusCode, &str) = match self {
                    #error_name::Forbidden => (::axum::http::StatusCode::FORBIDDEN, "forbidden"),
                    #error_name::NotFound => (::axum::http::StatusCode::NOT_FOUND, "not found"),
                    #error_name::BadRequest(ref m) => (::axum::http::StatusCode::BAD_REQUEST, m.as_str()),
                    #error_name::Conflict(ref m) => (::axum::http::StatusCode::CONFLICT, m.as_str()),
                    #error_name::Internal(ref m) => (::axum::http::StatusCode::INTERNAL_SERVER_ERROR, m.as_str()),
                };
                let body = ::axum::Json(::serde_json::json!({ "error": msg }));
                (status, body).into_response()
            }
        }

        impl #api {
            /// Build the CRUD router backed by `repo`, guarded by policy `A`.
            pub fn router<R, A>(repo: ::std::sync::Arc<R>) -> ::axum::Router
            where
                R: ::tpt_erp_entity::Repository<#entity_ty> + 'static,
                A: ::tpt_erp_entity::AuthPolicy,
            {
                ::axum::Router::new()
                    .route(#path, ::axum::routing::get(#list::<R, A>).post(#create::<R, A>))
                    .route(#path_id, ::axum::routing::get(#get_one::<R, A>).put(#replace::<R, A>).delete(#delete::<R, A>))
                    .layer(::axum::middleware::from_fn(#ensure_principal))
                    .with_state(#state_name::<R, A> { repo, _auth: ::core::marker::PhantomData })
            }
        }

        /// Middleware that guarantees a [`::tpt_erp_entity::Principal`] is available to the
        /// handlers, but only when an upstream auth middleware has not already inserted a
        /// real one. This keeps a custom [`::tpt_erp_entity::AuthPolicy`] authoritative:
        /// its `Principal` is never clobbered by a default.
        #[allow(non_snake_case)]
        async fn #ensure_principal(
            mut req: ::axum::extract::Request,
            next: ::axum::middleware::Next,
        ) -> ::axum::response::Response {
            if req.extensions().get::<::tpt_erp_entity::Principal>().is_none() {
                req.extensions_mut().insert(::tpt_erp_entity::Principal::default());
            }
            next.run(req).await
        }

        #[allow(non_snake_case)]
        async fn #list<R, A>(
            state: ::axum::extract::State<#state_name<R, A>>,
            filter: ::axum::extract::Query<<#entity_ty as ::tpt_erp_entity::EntityTable>::Filter>,
            principal: ::axum::extract::Extension<::tpt_erp_entity::Principal>,
        ) -> ::core::result::Result<::axum::Json<::tpt_erp_entity::Page<#entity_ty>>, #error_name>
        where
            R: ::tpt_erp_entity::Repository<#entity_ty> + 'static,
            A: ::tpt_erp_entity::AuthPolicy,
        {
            let principal = &principal.0;
            A::authorize(::tpt_erp_entity::Operation::List, principal)
                .map_err(|_| #error_name::Forbidden)?;
            let pagination = ::tpt_erp_entity::Pagination {
                page: filter.0.page.unwrap_or(1),
                per_page: filter.0.per_page.unwrap_or(20),
            };
            let page = state
                .0
                .repo
                .list(pagination, filter.0)
                .await
                .map_err(#error_name::from)?;
            ::core::result::Result::Ok(::axum::Json(page))
        }

        #[allow(non_snake_case)]
        async fn #get_one<R, A>(
            state: ::axum::extract::State<#state_name<R, A>>,
            id: ::axum::extract::Path<<#entity_ty as ::tpt_erp_entity::EntityTable>::Id>,
            principal: ::axum::extract::Extension<::tpt_erp_entity::Principal>,
        ) -> ::core::result::Result<::axum::Json<#entity_ty>, #error_name>
        where
            R: ::tpt_erp_entity::Repository<#entity_ty> + 'static,
            A: ::tpt_erp_entity::AuthPolicy,
        {
            let principal = &principal.0;
            A::authorize(::tpt_erp_entity::Operation::Read, principal)
                .map_err(|_| #error_name::Forbidden)?;
            match state.0.repo.get(id.0).await.map_err(#error_name::from)? {
                ::core::option::Option::Some(e) => ::core::result::Result::Ok(::axum::Json(e)),
                ::core::option::Option::None => ::core::result::Result::Err(#error_name::NotFound),
            }
        }

        #[allow(non_snake_case)]
        async fn #create<R, A>(
            state: ::axum::extract::State<#state_name<R, A>>,
            principal: ::axum::extract::Extension<::tpt_erp_entity::Principal>,
            body: ::axum::Json<#entity_ty>,
        ) -> ::core::result::Result<(::axum::http::StatusCode, ::axum::Json<#entity_ty>), #error_name>
        where
            R: ::tpt_erp_entity::Repository<#entity_ty> + 'static,
            A: ::tpt_erp_entity::AuthPolicy,
        {
            let principal = &principal.0;
            A::authorize(::tpt_erp_entity::Operation::Create, principal)
                .map_err(|_| #error_name::Forbidden)?;
            let created = state.0.repo.create(body.0).await.map_err(#error_name::from)?;
            ::core::result::Result::Ok((::axum::http::StatusCode::CREATED, ::axum::Json(created)))
        }

        #[allow(non_snake_case)]
        async fn #replace<R, A>(
            state: ::axum::extract::State<#state_name<R, A>>,
            id: ::axum::extract::Path<<#entity_ty as ::tpt_erp_entity::EntityTable>::Id>,
            principal: ::axum::extract::Extension<::tpt_erp_entity::Principal>,
            body: ::axum::Json<#entity_ty>,
        ) -> ::core::result::Result<::axum::Json<#entity_ty>, #error_name>
        where
            R: ::tpt_erp_entity::Repository<#entity_ty> + 'static,
            A: ::tpt_erp_entity::AuthPolicy,
        {
            let principal = &principal.0;
            A::authorize(::tpt_erp_entity::Operation::Update, principal)
                .map_err(|_| #error_name::Forbidden)?;
            match state.0.repo.replace(id.0, body.0).await.map_err(#error_name::from)? {
                ::core::option::Option::Some(e) => ::core::result::Result::Ok(::axum::Json(e)),
                ::core::option::Option::None => ::core::result::Result::Err(#error_name::NotFound),
            }
        }

        #[allow(non_snake_case)]
        async fn #delete<R, A>(
            state: ::axum::extract::State<#state_name<R, A>>,
            id: ::axum::extract::Path<<#entity_ty as ::tpt_erp_entity::EntityTable>::Id>,
            principal: ::axum::extract::Extension<::tpt_erp_entity::Principal>,
        ) -> ::core::result::Result<::axum::http::StatusCode, #error_name>
        where
            R: ::tpt_erp_entity::Repository<#entity_ty> + 'static,
            A: ::tpt_erp_entity::AuthPolicy,
        {
            let principal = &principal.0;
            A::authorize(::tpt_erp_entity::Operation::Delete, principal)
                .map_err(|_| #error_name::Forbidden)?;
            match state.0.repo.delete(id.0).await.map_err(#error_name::from)? {
                true => ::core::result::Result::Ok(::axum::http::StatusCode::NO_CONTENT),
                false => ::core::result::Result::Err(#error_name::NotFound),
            }
        }
    };

    Ok(expanded)
}
