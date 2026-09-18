//! Derives `Crud` for a `#[derive(sqlx::FromRow, flare_db::Crud)]` struct: generates
//! `{Entity}New`/`{Entity}Patch`/`{Entity}Filter` companion structs and an `impl`
//! block with `list`/`get`/`create_one`/`create_many`/`update_one`/`update_many`/
//! `count`/`list_and_count`/`delete` (or `soft_delete`/`restore` under
//! `#[crud(soft_delete)]`), plus the sea-query-based `list_where`/`update_where`/
//! `soft_delete_where`/`restore_where` filtered path. See flare-db's crate docs for
//! the full design rationale.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Type, parse_macro_input};

struct CrudConfig {
    table: LitStr,
    pk: Ident,
    soft_delete: bool,
}

fn parse_config(input: &DeriveInput) -> syn::Result<CrudConfig> {
    let mut table: Option<LitStr> = None;
    let mut pk: Option<Ident> = None;
    let mut soft_delete = false;

    let attr = input
        .attrs
        .iter()
        .find(|a| a.path().is_ident("crud"))
        .ok_or_else(|| {
            syn::Error::new_spanned(
                input,
                "#[derive(Crud)] requires a `#[crud(table = \"...\", pk = \"...\")]` attribute",
            )
        })?;

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("table") {
            let value = meta.value()?;
            table = Some(value.parse()?);
            Ok(())
        } else if meta.path.is_ident("pk") {
            let value = meta.value()?;
            let lit: LitStr = value.parse()?;
            pk = Some(Ident::new(&lit.value(), lit.span()));
            Ok(())
        } else if meta.path.is_ident("soft_delete") {
            soft_delete = true;
            Ok(())
        } else {
            Err(meta
                .error("unrecognized #[crud(...)] key — expected `table`, `pk`, or `soft_delete`"))
        }
    })?;

    Ok(CrudConfig {
        table: table.ok_or_else(|| {
            syn::Error::new_spanned(attr, "#[crud(...)] is missing required `table = \"...\"`")
        })?,
        pk: pk.ok_or_else(|| {
            syn::Error::new_spanned(attr, "#[crud(...)] is missing required `pk = \"...\"`")
        })?,
        soft_delete,
    })
}

struct FieldInfo {
    ident: Ident,
    ty: Type,
}

/// If `ty` is syntactically `Option<T>`, returns `T`.
fn unwrap_option(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

#[proc_macro_derive(Crud, attributes(crud))]
pub fn derive_crud(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let config = match parse_config(&input) {
        Ok(c) => c,
        Err(e) => return e.to_compile_error().into(),
    };

    let struct_name = &input.ident;

    let Data::Struct(data_struct) = &input.data else {
        return syn::Error::new_spanned(&input, "#[derive(Crud)] only supports structs")
            .to_compile_error()
            .into();
    };
    let Fields::Named(fields_named) = &data_struct.fields else {
        return syn::Error::new_spanned(&input, "#[derive(Crud)] requires named fields")
            .to_compile_error()
            .into();
    };

    let all_fields: Vec<FieldInfo> = fields_named
        .named
        .iter()
        .map(|f| FieldInfo {
            ident: f.ident.clone().expect("named field"),
            ty: f.ty.clone(),
        })
        .collect();

    let Some(pk_field) = all_fields.iter().find(|f| f.ident == config.pk) else {
        return syn::Error::new_spanned(
            &input,
            format!(
                "#[crud(pk = \"{}\")] does not match any field on this struct",
                config.pk
            ),
        )
        .to_compile_error()
        .into();
    };
    let pk_ident = pk_field.ident.clone();
    let pk_ty = pk_field.ty.clone();
    let pk_col = LitStr::new(&pk_ident.to_string(), pk_ident.span());

    // Soft-delete entities: `deleted_at` is excluded from the insertable/patchable field
    // set. Letting `New`/`Patch` set it directly would let a caller soft-delete (or
    // un-delete) a row through the generic patch path, bypassing `soft_delete`/`restore` —
    // the column stays reachable for reads/filtering via `{Entity}Filter`, just not writes.
    let other_fields: Vec<&FieldInfo> = all_fields
        .iter()
        .filter(|f| f.ident != config.pk && !(config.soft_delete && f.ident == "deleted_at"))
        .collect();

    let deleted_at_inner_ty: Option<Type> = if config.soft_delete {
        match all_fields.iter().find(|f| f.ident == "deleted_at") {
            Some(f) => match unwrap_option(&f.ty) {
                Some(inner) => Some(inner.clone()),
                None => {
                    return syn::Error::new_spanned(
                        &f.ty,
                        "#[crud(soft_delete)] requires `deleted_at` to be `Option<T>`",
                    )
                    .to_compile_error()
                    .into();
                }
            },
            None => {
                return syn::Error::new_spanned(
                    &input,
                    "#[crud(soft_delete)] requires a `deleted_at: Option<T>` field",
                )
                .to_compile_error()
                .into();
            }
        }
    } else {
        None
    };

    let table_lit = &config.table;

    let all_col_lits: Vec<LitStr> = all_fields
        .iter()
        .map(|f| LitStr::new(&f.ident.to_string(), f.ident.span()))
        .collect();
    let all_field_idents: Vec<Ident> = all_fields.iter().map(|f| f.ident.clone()).collect();
    let all_field_tys: Vec<Type> = all_fields.iter().map(|f| f.ty.clone()).collect();

    let other_col_lits: Vec<LitStr> = other_fields
        .iter()
        .map(|f| LitStr::new(&f.ident.to_string(), f.ident.span()))
        .collect();
    let other_field_idents: Vec<Ident> = other_fields.iter().map(|f| f.ident.clone()).collect();
    let other_field_tys: Vec<Type> = other_fields.iter().map(|f| f.ty.clone()).collect();

    let new_ident = format_ident!("{}New", struct_name);
    let patch_ident = format_ident!("{}Patch", struct_name);
    let filter_ident = format_ident!("{}Filter", struct_name);

    let new_struct = quote! {
        pub struct #new_ident {
            #(pub #other_field_idents: #other_field_tys,)*
        }
    };

    let patch_struct = quote! {
        #[derive(Default)]
        pub struct #patch_ident {
            #(pub #other_field_idents: Option<#other_field_tys>,)*
        }
    };

    let filter_struct = quote! {
        #[derive(Default)]
        pub struct #filter_ident {
            #(pub #all_field_idents: Option<flare_db::FilterOp<#all_field_tys>>,)*
        }
    };

    let soft_delete_read_guard = if config.soft_delete {
        quote! { q.and_where(flare_db::sea_query::Expr::col("deleted_at").is_null()); }
    } else {
        quote! {}
    };
    let soft_delete_read_guard_count = soft_delete_read_guard.clone();

    let delete_methods = if config.soft_delete {
        let deleted_inner_ty = deleted_at_inner_ty.expect("checked above");
        quote! {
            pub async fn soft_delete(pool: &flare_db::Pool, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<()> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table_lit)
                    .value("deleted_at", flare_db::sea_query::Expr::current_timestamp())
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(())
            }

            pub async fn restore(pool: &flare_db::Pool, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<()> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table_lit)
                    .value("deleted_at", flare_db::sea_query::Value::from(None::<#deleted_inner_ty>))
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(())
            }

            pub async fn soft_delete_where(pool: &flare_db::Pool, filter: #filter_ident) -> flare_db::sqlx::Result<u64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table_lit).value("deleted_at", flare_db::sea_query::Expr::current_timestamp());
                #(q.and_where_option(filter.#all_field_idents.map(|op| flare_db::FilterOp::into_expr(op, #all_col_lits)));)*
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let result = flare_db::sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(result.rows_affected())
            }

            pub async fn restore_where(pool: &flare_db::Pool, filter: #filter_ident) -> flare_db::sqlx::Result<u64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table_lit).value("deleted_at", flare_db::sea_query::Value::from(None::<#deleted_inner_ty>));
                #(q.and_where_option(filter.#all_field_idents.map(|op| flare_db::FilterOp::into_expr(op, #all_col_lits)));)*
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let result = flare_db::sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(result.rows_affected())
            }
        }
    } else {
        quote! {
            pub async fn delete(pool: &flare_db::Pool, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<()> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::delete();
                q.from_table(#table_lit)
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(())
            }
        }
    };

    let expanded = quote! {
        #new_struct
        #patch_struct
        #filter_struct

        impl #struct_name {
            pub async fn get(pool: &flare_db::Pool, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<Option<Self>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::select();
                q.columns([#(#all_col_lits),*])
                    .from(#table_lit)
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                #soft_delete_read_guard
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values).fetch_optional(pool).await
            }

            pub async fn list(pool: &flare_db::Pool, page: flare_db::Page) -> flare_db::sqlx::Result<Vec<Self>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::select();
                q.columns([#(#all_col_lits),*]).from(#table_lit);
                #soft_delete_read_guard
                page.apply(&mut q);
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values).fetch_all(pool).await
            }

            pub async fn list_and_count(pool: &flare_db::Pool, page: flare_db::Page) -> flare_db::sqlx::Result<(Vec<Self>, i64)> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut tx = pool.begin().await?;

                let mut q = flare_db::sea_query::Query::select();
                q.columns([#(#all_col_lits),*]).from(#table_lit);
                #soft_delete_read_guard
                page.apply(&mut q);
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let rows = flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values)
                    .fetch_all(&mut *tx)
                    .await?;

                let mut cq = flare_db::sea_query::Query::select();
                cq.expr(flare_db::sea_query::Func::count(flare_db::sea_query::Expr::col(#pk_col)))
                    .from(#table_lit);
                #soft_delete_read_guard_count
                let (csql, cvalues) = cq.build_sqlx(flare_db::QUERY_BUILDER);
                let count: i64 = flare_db::sqlx::query_scalar_with(&csql, cvalues)
                    .fetch_one(&mut *tx)
                    .await?;

                tx.commit().await?;
                Ok((rows, count))
            }

            pub async fn count(pool: &flare_db::Pool) -> flare_db::sqlx::Result<i64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::select();
                q.expr(flare_db::sea_query::Func::count(flare_db::sea_query::Expr::col(#pk_col)))
                    .from(#table_lit);
                #soft_delete_read_guard
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_scalar_with(&sql, values).fetch_one(pool).await
            }

            pub async fn create_one(pool: &flare_db::Pool, new: #new_ident) -> flare_db::sqlx::Result<Self> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::insert();
                q.into_table(#table_lit).columns([#(#other_col_lits),*]);
                q.values_panic([#(flare_db::sea_query::SimpleExpr::from(new.#other_field_idents)),*]);
                q.returning_all();
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values).fetch_one(pool).await
            }

            pub async fn create_many(pool: &flare_db::Pool, news: Vec<#new_ident>) -> flare_db::sqlx::Result<Vec<Self>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                if news.is_empty() {
                    return Ok(Vec::new());
                }
                let mut q = flare_db::sea_query::Query::insert();
                q.into_table(#table_lit).columns([#(#other_col_lits),*]);
                for new in news {
                    q.values_panic([#(flare_db::sea_query::SimpleExpr::from(new.#other_field_idents)),*]);
                }
                q.returning_all();
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values).fetch_all(pool).await
            }

            pub async fn update_one(pool: &flare_db::Pool, #pk_ident: #pk_ty, patch: #patch_ident) -> flare_db::sqlx::Result<Self> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table_lit);
                let mut has_set = false;
                #(
                    if let Some(v) = patch.#other_field_idents {
                        q.value(#other_col_lits, v);
                        has_set = true;
                    }
                )*
                if !has_set {
                    return Self::get(pool, #pk_ident)
                        .await?
                        .ok_or(flare_db::sqlx::Error::RowNotFound);
                }
                q.and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                q.returning_all();
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values).fetch_one(pool).await
            }

            pub async fn update_many(
                pool: &flare_db::Pool,
                patches: Vec<(#pk_ty, #patch_ident)>,
            ) -> flare_db::sqlx::Result<Vec<Self>> {
                let mut out = Vec::with_capacity(patches.len());
                for (id, patch) in patches {
                    out.push(Self::update_one(pool, id, patch).await?);
                }
                Ok(out)
            }

            pub async fn list_where(
                pool: &flare_db::Pool,
                filter: #filter_ident,
                page: flare_db::Page,
            ) -> flare_db::sqlx::Result<Vec<Self>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::select();
                q.columns([#(#all_col_lits),*]).from(#table_lit);
                #(q.and_where_option(filter.#all_field_idents.map(|op| flare_db::FilterOp::into_expr(op, #all_col_lits)));)*
                page.apply(&mut q);
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, Self, _>(&sql, values).fetch_all(pool).await
            }

            pub async fn update_where(
                pool: &flare_db::Pool,
                filter: #filter_ident,
                patch: #patch_ident,
            ) -> flare_db::sqlx::Result<u64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table_lit);
                let mut has_set = false;
                #(
                    if let Some(v) = patch.#other_field_idents {
                        q.value(#other_col_lits, v);
                        has_set = true;
                    }
                )*
                if !has_set {
                    return Ok(0);
                }
                #(q.and_where_option(filter.#all_field_idents.map(|op| flare_db::FilterOp::into_expr(op, #all_col_lits)));)*
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let result = flare_db::sqlx::query_with(&sql, values).execute(pool).await?;
                Ok(result.rows_affected())
            }

            #delete_methods
        }
    };

    expanded.into()
}
