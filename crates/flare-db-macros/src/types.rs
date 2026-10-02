//! Generated companion types: `{E}New`, `{E}Patch`, `{E}Filter`, `{E}Field`,
//! `{E}Partial` and (when the entity has `hidden`/`computed` fields) `{E}Public`.

use crate::model::{Model, col_lit, pascal};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

pub struct Names {
    pub new: syn::Ident,
    pub patch: syn::Ident,
    pub filter: syn::Ident,
    pub field: syn::Ident,
    pub partial: syn::Ident,
    pub public: syn::Ident,
}

impl Names {
    pub fn new(m: &Model) -> Self {
        let n = &m.name;
        Self {
            new: format_ident!("{}New", n),
            patch: format_ident!("{}Patch", n),
            filter: format_ident!("{}Filter", n),
            field: format_ident!("{}Field", n),
            partial: format_ident!("{}Partial", n),
            public: format_ident!("{}Public", n),
        }
    }
}

pub fn gen_types(m: &Model, n: &Names) -> TokenStream {
    let (new_ty, patch_ty, filter_ty) = (&n.new, &n.patch, &n.filter);

    let new_fields = m.new_fields();
    let new_idents = new_fields.iter().map(|f| &f.ident);
    // `id(..)` and `default = ..` fields become optional on insert.
    let new_tys = new_fields.iter().map(|f| {
        let ty = &f.ty;
        if f.attrs.id_prefix.is_some() || f.attrs.default.is_some() {
            quote! { Option<#ty> }
        } else {
            quote! { #ty }
        }
    });

    let patch_fields = m.patch_fields();
    let patch_idents = patch_fields.iter().map(|f| &f.ident);
    let patch_tys = patch_fields.iter().map(|f| &f.ty);

    let columns = m.columns();
    let filter_idents = columns.iter().map(|f| &f.ident);
    let filter_tys = columns.iter().map(|f| &f.ty);
    let q_field = if m.searchable().is_empty() {
        quote! {}
    } else {
        quote! {
            /// Case-insensitive substring search OR-ed over the `searchable` fields.
            /// `%`, `_` and `\` in the text match literally. Blank = no restriction.
            pub q: Option<String>,
        }
    };
    let with_deleted_field = if m.cfg.soft_delete {
        quote! {
            /// Reads exclude soft-deleted rows unless this is set or `deleted_at` is
            /// filtered explicitly. Only the top-level filter's flag is consulted.
            pub with_deleted: bool,
        }
    } else {
        quote! {}
    };

    let visible = m.visible();
    let variants: Vec<_> = visible.iter().map(|f| pascal(&f.ident)).collect();
    let vis_cols: Vec<_> = visible.iter().map(|f| col_lit(f)).collect();
    let field_ty = &n.field;

    let vis_idents: Vec<_> = visible.iter().map(|f| &f.ident).collect();
    let vis_tys: Vec<_> = visible.iter().map(|f| &f.ty).collect();
    let partial_ty = &n.partial;

    let public = if m.needs_public() {
        let public_ty = &n.public;
        let shown: Vec<_> = m.fields.iter().filter(|f| !f.attrs.hidden).collect();
        let p_idents: Vec<_> = shown.iter().map(|f| &f.ident).collect();
        let p_tys: Vec<_> = shown.iter().map(|f| &f.ty).collect();
        let reads = shown.iter().map(|f| {
            let id = &f.ident;
            if f.attrs.computed {
                quote! { #id: ::core::default::Default::default() }
            } else {
                let col = col_lit(f);
                quote! { #id: row.try_get(#col)? }
            }
        });
        quote! {
            /// Output type of every read/write return path: the entity minus its
            /// `hidden` fields (computed fields are filled with `Default`).
            pub struct #public_ty {
                #(pub #p_idents: #p_tys,)*
            }
            impl<'r> flare_db::sqlx::FromRow<'r, flare_db::Row> for #public_ty {
                fn from_row(row: &'r flare_db::Row) -> flare_db::sqlx::Result<Self> {
                    use flare_db::sqlx::Row as _;
                    Ok(Self { #(#reads,)* })
                }
            }
        }
    } else {
        quote! {}
    };

    quote! {
        pub struct #new_ty {
            #(pub #new_idents: #new_tys,)*
        }

        #[derive(Default)]
        pub struct #patch_ty {
            #(pub #patch_idents: Option<#patch_tys>,)*
        }

        #[derive(Default)]
        pub struct #filter_ty {
            #(pub #filter_idents: Option<flare_db::FilterOp<#filter_tys>>,)*
            #q_field
            #with_deleted_field
            /// Every sub-filter must match (AND).
            pub and: Vec<#filter_ty>,
            /// At least one sub-filter must match (OR). Empty sub-filters are ignored.
            pub or: Vec<#filter_ty>,
        }

        /// Selectable (non-`hidden`, non-`computed`) columns, for `get_select`/`list_select`
        /// and `upsert_*_on`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum #field_ty {
            #(#variants,)*
        }

        impl #field_ty {
            pub const fn as_str(self) -> &'static str {
                match self {
                    #(Self::#variants => #vis_cols,)*
                }
            }
        }

        /// Result of a projected read: only the requested columns are `Some`
        /// (a selected NULL on a nullable column is `Some(None)`).
        pub struct #partial_ty {
            #(pub #vis_idents: Option<#vis_tys>,)*
        }

        impl<'r> flare_db::sqlx::FromRow<'r, flare_db::Row> for #partial_ty {
            fn from_row(row: &'r flare_db::Row) -> flare_db::sqlx::Result<Self> {
                use flare_db::sqlx::Row as _;
                Ok(Self {
                    #(#vis_idents: match row.try_get::<#vis_tys, _>(#vis_cols) {
                        Ok(v) => Some(v),
                        Err(flare_db::sqlx::Error::ColumnNotFound(_)) => None,
                        Err(e) => return Err(e),
                    },)*
                })
            }
        }

        #public
    }
}
