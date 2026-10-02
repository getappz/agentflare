//! The generated `impl Entity { ... }` block.
//!
//! SQLx 0.9 requires an explicit audit of dynamic SQL. Every query below is built by
//! SeaQuery: identifiers are quoted and all runtime values are bound separately. Do
//! not replace these builders with interpolated SQL or custom expressions.
//!
//! Output shaping lives in one place: every SELECT list and RETURNING clause is
//! `Self::COLUMNS` (visible columns only) and every row decodes into `Self::Row`'s
//! type (`Self`, or `{E}Public` when the entity has hidden/computed fields).

use crate::model::{Model, col_lit};
use crate::types::Names;
use proc_macro2::TokenStream;
use quote::quote;

pub fn gen_impl(m: &Model, n: &Names) -> TokenStream {
    let struct_name = &m.name;
    let table = &m.cfg.table;
    let pk = m.pk();
    let (pk_ident, pk_ty) = (&pk.ident, &pk.ty);
    let pk_col = col_lit(pk);
    let (new_ty, patch_ty, filter_ty, field_ty, partial_ty) =
        (&n.new, &n.patch, &n.filter, &n.field, &n.partial);
    let out_ty = if m.needs_public() {
        let p = &n.public;
        quote! { #p }
    } else {
        quote! { Self }
    };

    let vis_cols: Vec<_> = m.visible().iter().map(|f| col_lit(f)).collect();
    let all_cols = m.columns();
    let filter_idents: Vec<_> = all_cols.iter().map(|f| &f.ident).collect();
    let filter_cols: Vec<_> = all_cols.iter().map(|f| col_lit(f)).collect();

    // ---- insert side -------------------------------------------------------------
    let insert_fields = m.insert_fields();
    let insert_cols: Vec<_> = insert_fields.iter().map(|f| col_lit(f)).collect();
    let insert_values = insert_fields.iter().map(|f| {
        let id = &f.ident;
        if f.attrs.created_at || f.attrs.updated_at {
            quote! { flare_db::sea_query::SimpleExpr::from(now) }
        } else if let Some(prefix) = &f.attrs.id_prefix {
            quote! {
                flare_db::sea_query::SimpleExpr::from(
                    new.#id.unwrap_or_else(|| flare_db::generate_id(#prefix).into()),
                )
            }
        } else if let Some(default) = &f.attrs.default {
            quote! { flare_db::sea_query::SimpleExpr::from(new.#id.unwrap_or_else(|| #default)) }
        } else {
            quote! { flare_db::sea_query::SimpleExpr::from(new.#id) }
        }
    });

    // ---- validation --------------------------------------------------------------
    let new_checks = m
        .new_fields()
        .into_iter()
        .filter(|f| !f.attrs.enum_values.is_empty())
        .map(|f| {
            let (id, col, choices) = (&f.ident, col_lit(f), &f.attrs.enum_values);
            quote! { flare_db::check_choice(#col, &new.#id, &[#(#choices),*])?; }
        });
    let patch_checks = m
        .patch_fields()
        .into_iter()
        .filter(|f| !f.attrs.enum_values.is_empty())
        .map(|f| {
            let (id, col, choices) = (&f.ident, col_lit(f), &f.attrs.enum_values);
            quote! {
                if let Some(v) = &patch.#id {
                    flare_db::check_choice(#col, v, &[#(#choices),*])?;
                }
            }
        });
    let (hook_new, hook_patch) = if m.cfg.validate {
        (
            quote! { flare_db::Validate::validate(new)?; },
            quote! { flare_db::Validate::validate(patch)?; },
        )
    } else {
        (quote! {}, quote! {})
    };

    // ---- updates -----------------------------------------------------------------
    let patch_fields = m.patch_fields();
    let patch_idents: Vec<_> = patch_fields.iter().map(|f| &f.ident).collect();
    let patch_cols: Vec<_> = patch_fields.iter().map(|f| col_lit(f)).collect();
    let touch_updated_at = match m.updated_at() {
        Some(col) => quote! { q.value(#col, flare_db::now()); },
        None => quote! {},
    };
    let upsert_base_cols = m.upsert_base_cols();
    let upsert_defaults = m.upsert_default_fields();
    let default_idents: Vec<_> = upsert_defaults.iter().map(|f| &f.ident).collect();
    let default_cols: Vec<_> = upsert_defaults.iter().map(|f| col_lit(f)).collect();
    let default_bits: Vec<u32> = (0..default_idents.len() as u32).collect();

    // ---- filter ------------------------------------------------------------------
    let q_clause = {
        let searchable = m.searchable();
        if searchable.is_empty() {
            quote! {}
        } else {
            quote! {
                if let Some(term) = &filter.q {
                    if let Some(search) = flare_db::search_condition(&[#(#searchable),*], term) {
                        cond = cond.add(search);
                    }
                }
            }
        }
    };
    let read_guard = if m.cfg.soft_delete {
        quote! {
            let unguarded = filter.with_deleted || filter.deleted_at.is_some();
            let mut cond = Self::__where(filter);
            if !unguarded {
                cond = cond.add(flare_db::sea_query::Expr::col("deleted_at").is_null());
            }
            cond
        }
    } else {
        quote! { Self::__where(filter) }
    };
    let soft_guard_stmt = if m.cfg.soft_delete {
        quote! { q.and_where(flare_db::sea_query::Expr::col("deleted_at").is_null()); }
    } else {
        quote! {}
    };

    // ---- delete / restore --------------------------------------------------------
    let delete_methods = if m.cfg.soft_delete {
        let inner = m.deleted_inner.as_ref().expect("checked in Model::new");
        quote! {
            pub async fn soft_delete<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<()> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let evt_id = #pk_ident.to_string();
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table)
                    .value("deleted_at", flare_db::sea_query::Expr::current_timestamp())
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                // Only a live row changes state: an already-deleted row keeps its
                // original `deleted_at` and `updated_at`, and emits no event.
                #soft_guard_stmt
                #touch_updated_at
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let done = flare_db::sqlx::query_with(flare_db::sqlx::AssertSqlSafe(sql), values).execute(pool).await?;
                if done.rows_affected() > 0 {
                    flare_db::events::emit(#table, flare_db::MutationKind::Deleted, Some(evt_id));
                }
                Ok(())
            }

            pub async fn restore<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<()> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let evt_id = #pk_ident.to_string();
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table)
                    .value("deleted_at", flare_db::sea_query::Value::from(None::<#inner>))
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                // Only a deleted row is restored; a live row is left untouched.
                q.and_where(flare_db::sea_query::Expr::col("deleted_at").is_not_null());
                #touch_updated_at
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let done = flare_db::sqlx::query_with(flare_db::sqlx::AssertSqlSafe(sql), values).execute(pool).await?;
                if done.rows_affected() > 0 {
                    flare_db::events::emit(#table, flare_db::MutationKind::Restored, Some(evt_id));
                }
                Ok(())
            }

            pub async fn soft_delete_where<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, filter: #filter_ty) -> flare_db::sqlx::Result<u64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table).value("deleted_at", flare_db::sea_query::Expr::current_timestamp());
                q.cond_where(Self::__where(filter));
                #soft_guard_stmt
                #touch_updated_at
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let done = flare_db::sqlx::query_with(flare_db::sqlx::AssertSqlSafe(sql), values).execute(pool).await?;
                if done.rows_affected() > 0 {
                    flare_db::events::emit(#table, flare_db::MutationKind::Deleted, None);
                }
                Ok(done.rows_affected())
            }

            pub async fn restore_where<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, filter: #filter_ty) -> flare_db::sqlx::Result<u64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table).value("deleted_at", flare_db::sea_query::Value::from(None::<#inner>));
                q.cond_where(Self::__where(filter));
                q.and_where(flare_db::sea_query::Expr::col("deleted_at").is_not_null());
                #touch_updated_at
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let done = flare_db::sqlx::query_with(flare_db::sqlx::AssertSqlSafe(sql), values).execute(pool).await?;
                if done.rows_affected() > 0 {
                    flare_db::events::emit(#table, flare_db::MutationKind::Restored, None);
                }
                Ok(done.rows_affected())
            }
        }
    } else {
        quote! {
            pub async fn delete<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<()> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let evt_id = #pk_ident.to_string();
                let mut q = flare_db::sea_query::Query::delete();
                q.from_table(#table)
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let done = flare_db::sqlx::query_with(flare_db::sqlx::AssertSqlSafe(sql), values).execute(pool).await?;
                if done.rows_affected() > 0 {
                    flare_db::events::emit(#table, flare_db::MutationKind::Deleted, Some(evt_id));
                }
                Ok(())
            }
        }
    };

    // ---- upsert_one / upsert_many with the declared `unique(..)` target -----------
    let unique_upserts = if m.cfg.unique.is_empty() {
        quote! {}
    } else {
        let cols: Vec<String> = m.cfg.unique.iter().map(|i| i.to_string()).collect();
        quote! {
            /// Upserts on the `#[crud(unique(..))]` columns.
            pub async fn upsert_many<'e>(pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>, news: Vec<#new_ty>) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                Self::__upsert(pool, &[#(#cols),*], news).await
            }

            pub async fn upsert_one<'e>(pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>, new: #new_ty) -> flare_db::sqlx::Result<#out_ty> {
                Ok(Self::__upsert(pool, &[#(#cols),*], vec![new]).await?.remove(0))
            }
        }
    };

    let ts_ty_checks = m
        .fields
        .iter()
        .filter(|f| f.attrs.created_at || f.attrs.updated_at)
        .map(|f| {
            let ty = &f.ty;
            quote! { flare_db::assert_timestamp::<#ty>(); }
        });

    quote! {
        #[allow(dead_code)]
        const _: () = {
            // created_at/updated_at are bound as OffsetDateTime: reject other types here.
            fn __timestamp_fields() {
                #(#ts_ty_checks)*
            }
        };

        #[allow(unused_mut, unused_variables, unused_imports)]
        impl #struct_name {
            /// Columns that are ever selected or returned (no `hidden`/`computed`).
            pub const COLUMNS: &'static [&'static str] = &[#(#vis_cols),*];

            fn __returning() -> flare_db::sea_query::ReturningClause {
                flare_db::sea_query::Query::returning().columns(Self::COLUMNS.iter().copied())
            }

            fn __validate_new(new: &#new_ty) -> ::core::result::Result<(), flare_db::ValidationError> {
                #(#new_checks)*
                #hook_new
                Ok(())
            }

            fn __validate_patch(patch: &#patch_ty) -> ::core::result::Result<(), flare_db::ValidationError> {
                #(#patch_checks)*
                #hook_patch
                Ok(())
            }

            fn __insert_row(new: #new_ty, now: flare_db::time::OffsetDateTime) -> Vec<flare_db::sea_query::SimpleExpr> {
                vec![#(#insert_values),*]
            }

            /// Every filter predicate, AND-ed (plus `q`, `and`, `or`).
            fn __where(filter: #filter_ty) -> flare_db::sea_query::Condition {
                use flare_db::sea_query::ExprTrait as _;
                let mut cond = flare_db::sea_query::Condition::all();
                #(
                    if let Some(op) = filter.#filter_idents {
                        cond = cond.add(flare_db::FilterOp::into_expr(op, #filter_cols));
                    }
                )*
                #q_clause
                for sub in filter.and {
                    let sub = Self::__where(sub);
                    if !sub.is_empty() {
                        cond = cond.add(sub);
                    }
                }
                let mut any = flare_db::sea_query::Condition::any();
                for sub in filter.or {
                    let sub = Self::__where(sub);
                    if !sub.is_empty() {
                        any = any.add(sub);
                    }
                }
                if !any.is_empty() {
                    cond = cond.add(any);
                }
                cond
            }

            /// `__where` plus the soft-delete guard used by every read.
            fn __read_cond(filter: #filter_ty) -> flare_db::sea_query::Condition {
                use flare_db::sea_query::ExprTrait as _;
                #read_guard
            }

            fn __select_stmt(
                cols: &[&'static str],
                cond: flare_db::sea_query::Condition,
                page: flare_db::Page,
            ) -> flare_db::sea_query::SelectStatement {
                let mut q = flare_db::sea_query::Query::select();
                q.columns(cols.iter().copied()).from(#table);
                q.cond_where(cond);
                page.apply(&mut q);
                q
            }

            fn __projection(fields: &[#field_ty]) -> Vec<&'static str> {
                if fields.is_empty() {
                    Self::COLUMNS.to_vec()
                } else {
                    fields.iter().map(|f| f.as_str()).collect()
                }
            }

            // ---- reads -------------------------------------------------------------

            pub async fn get<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty) -> flare_db::sqlx::Result<Option<#out_ty>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let mut q = flare_db::sea_query::Query::select();
                q.columns(Self::COLUMNS.iter().copied())
                    .from(#table)
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                #soft_guard_stmt
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, #out_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_optional(pool).await
            }

            /// Like `get`, but only the given columns are selected (empty = all visible).
            pub async fn get_select<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty, fields: &[#field_ty]) -> flare_db::sqlx::Result<Option<#partial_ty>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let mut q = flare_db::sea_query::Query::select();
                q.columns(Self::__projection(fields))
                    .from(#table)
                    .and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                #soft_guard_stmt
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, #partial_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_optional(pool).await
            }

            pub async fn list<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, page: flare_db::Page) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                Self::list_where(pool, #filter_ty::default(), page).await
            }

            /// Filtered read. Under `soft_delete`, deleted rows are excluded unless
            /// `filter.with_deleted` is set or `filter.deleted_at` is given.
            pub async fn list_where<'e>(
                pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>,
                filter: #filter_ty,
                page: flare_db::Page,
            ) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let q = Self::__select_stmt(Self::COLUMNS, Self::__read_cond(filter), page);
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, #out_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_all(pool).await
            }

            /// Filtered read of selected columns only (empty `fields` = all visible).
            pub async fn list_select<'e>(
                pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>,
                filter: #filter_ty,
                page: flare_db::Page,
                fields: &[#field_ty],
            ) -> flare_db::sqlx::Result<Vec<#partial_ty>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let q = Self::__select_stmt(&Self::__projection(fields), Self::__read_cond(filter), page);
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, #partial_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_all(pool).await
            }

            pub async fn list_and_count<'e>(pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>, page: flare_db::Page) -> flare_db::sqlx::Result<(Vec<#out_ty>, i64)> {
                Self::list_and_count_where(pool, #filter_ty::default(), page).await
            }

            /// Rows of the page plus the total matching `filter` (ignoring `page`).
            pub async fn list_and_count_where<'e>(
                pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>,
                filter: #filter_ty,
                page: flare_db::Page,
            ) -> flare_db::sqlx::Result<(Vec<#out_ty>, i64)> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                let mut tx = pool.begin().await?;
                let cond = Self::__read_cond(filter);

                let q = Self::__select_stmt(Self::COLUMNS, cond.clone(), page);
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let rows = flare_db::sqlx::query_as_with::<_, #out_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values)
                    .fetch_all(&mut *tx)
                    .await?;

                let mut cq = flare_db::sea_query::Query::select();
                cq.expr(flare_db::sea_query::Func::count(flare_db::sea_query::Expr::col(#pk_col)))
                    .from(#table);
                cq.cond_where(cond);
                let (csql, cvalues) = cq.build_sqlx(flare_db::QUERY_BUILDER);
                let count: i64 = flare_db::sqlx::query_scalar_with(flare_db::sqlx::AssertSqlSafe(csql), cvalues)
                    .fetch_one(&mut *tx)
                    .await?;

                tx.commit().await?;
                Ok((rows, count))
            }

            pub async fn count<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>) -> flare_db::sqlx::Result<i64> {
                Self::count_where(pool, #filter_ty::default()).await
            }

            pub async fn count_where<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, filter: #filter_ty) -> flare_db::sqlx::Result<i64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                let mut q = flare_db::sea_query::Query::select();
                q.expr(flare_db::sea_query::Func::count(flare_db::sea_query::Expr::col(#pk_col)))
                    .from(#table);
                q.cond_where(Self::__read_cond(filter));
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_scalar_with(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_one(pool).await
            }

            // ---- writes ------------------------------------------------------------

            pub async fn create_one<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, new: #new_ty) -> flare_db::sqlx::Result<#out_ty> {
                Ok(Self::create_many(pool, vec![new]).await?.remove(0))
            }

            pub async fn create_many<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, news: Vec<#new_ty>) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                if news.is_empty() {
                    return Ok(Vec::new());
                }
                for new in &news {
                    Self::__validate_new(new)?;
                }
                let now = flare_db::now();
                let mut q = flare_db::sea_query::Query::insert();
                q.into_table(#table).columns([#(#insert_cols),*]);
                for new in news {
                    q.values_panic(Self::__insert_row(new, now));
                }
                q.returning(Self::__returning());
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let rows = flare_db::sqlx::query_as_with::<_, #out_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_all(pool).await?;
                for row in &rows {
                    flare_db::events::emit(#table, flare_db::MutationKind::Created, Some(row.#pk_ident.to_string()));
                }
                Ok(rows)
            }

            async fn __update_one<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty, patch: #patch_ty) -> flare_db::sqlx::Result<#out_ty> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                use flare_db::sea_query::ExprTrait as _;
                Self::__validate_patch(&patch)?;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table);
                let mut has_set = false;
                #(
                    if let Some(v) = patch.#patch_idents {
                        q.value(#patch_cols, v);
                        has_set = true;
                    }
                )*
                if !has_set {
                    return Self::get(pool, #pk_ident)
                        .await?
                        .ok_or(flare_db::sqlx::Error::RowNotFound);
                }
                #touch_updated_at
                q.and_where(flare_db::sea_query::Expr::col(#pk_col).eq(#pk_ident));
                // Reads hide soft-deleted rows, so a patch must not reach one either.
                #soft_guard_stmt
                q.returning(Self::__returning());
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                flare_db::sqlx::query_as_with::<_, #out_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values).fetch_one(pool).await
            }

            pub async fn update_one<'e>(pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>, #pk_ident: #pk_ty, patch: #patch_ty) -> flare_db::sqlx::Result<#out_ty> {
                let row = Self::__update_one(pool, #pk_ident, patch).await?;
                flare_db::events::emit(#table, flare_db::MutationKind::Updated, Some(row.#pk_ident.to_string()));
                Ok(row)
            }

            pub async fn update_many<'e>(
                pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>,
                patches: Vec<(#pk_ty, #patch_ty)>,
            ) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                let mut tx = pool.begin().await?;
                let mut out = Vec::with_capacity(patches.len());
                for (id, patch) in patches {
                    out.push(Self::__update_one(&mut *tx, id, patch).await?);
                }
                tx.commit().await?;
                for row in &out {
                    flare_db::events::emit(#table, flare_db::MutationKind::Updated, Some(row.#pk_ident.to_string()));
                }
                Ok(out)
            }

            pub async fn update_where<'e>(
                pool: impl flare_db::sqlx::Executor<'e, Database = flare_db::Database>,
                filter: #filter_ty,
                patch: #patch_ty,
            ) -> flare_db::sqlx::Result<u64> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                Self::__validate_patch(&patch)?;
                let mut q = flare_db::sea_query::Query::update();
                q.table(#table);
                let mut has_set = false;
                #(
                    if let Some(v) = patch.#patch_idents {
                        q.value(#patch_cols, v);
                        has_set = true;
                    }
                )*
                if !has_set {
                    return Ok(0);
                }
                #touch_updated_at
                // Same visibility as reads: soft-deleted rows are skipped unless the
                // filter opts in (`with_deleted` or an explicit `deleted_at`).
                q.cond_where(Self::__read_cond(filter));
                let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                let done = flare_db::sqlx::query_with(flare_db::sqlx::AssertSqlSafe(sql), values).execute(pool).await?;
                if done.rows_affected() > 0 {
                    flare_db::events::emit(#table, flare_db::MutationKind::Updated, None);
                }
                Ok(done.rows_affected())
            }

            /// Rows are grouped by which `default` fields the caller supplied, one
            /// statement per group inside one transaction, so an unset default never
            /// clobbers a stored value while an explicit one does. Results follow group
            /// order, not input order.
            async fn __upsert<'e>(
                pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>,
                target: &[&'static str],
                news: Vec<#new_ty>,
            ) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                use flare_db::sea_query_binder::SqlxBinder as _;
                if news.is_empty() {
                    return Ok(Vec::new());
                }
                if target.is_empty() {
                    return Err(flare_db::ValidationError::new("upsert needs at least one conflict column").into());
                }
                for new in &news {
                    Self::__validate_new(new)?;
                }
                let mut groups: Vec<(u64, Vec<#new_ty>)> = Vec::new();
                for new in news {
                    let mask: u64 = 0u64 #( | ((new.#default_idents.is_some() as u64) << #default_bits) )*;
                    match groups.iter_mut().find(|(m, _)| *m == mask) {
                        Some((_, rows)) => rows.push(new),
                        None => groups.push((mask, vec![new])),
                    }
                }
                let now = flare_db::now();
                let mut tx = pool.begin().await?;
                let mut out = Vec::new();
                for (mask, rows) in groups {
                    let mut q = flare_db::sea_query::Query::insert();
                    q.into_table(#table).columns([#(#insert_cols),*]);
                    for new in rows {
                        q.values_panic(Self::__insert_row(new, now));
                    }
                    // Only patchable (never readonly/immutable/pk/created_at) columns are
                    // overwritten, plus updated_at. If nothing qualifies, rewrite the
                    // conflict key onto itself so RETURNING still yields the existing row.
                    let mut update: Vec<&'static str> = vec![#(#upsert_base_cols),*];
                    #( if mask & (1u64 << #default_bits) != 0 { update.push(#default_cols); } )*
                    update.retain(|c| !target.contains(c));
                    if update.is_empty() {
                        update.push(target[0]);
                    }
                    let mut on_conflict = flare_db::sea_query::OnConflict::columns(target.iter().copied());
                    on_conflict.update_columns(update);
                    q.on_conflict(on_conflict);
                    q.returning(Self::__returning());
                    let (sql, values) = q.build_sqlx(flare_db::QUERY_BUILDER);
                    out.extend(
                        flare_db::sqlx::query_as_with::<_, #out_ty, _>(flare_db::sqlx::AssertSqlSafe(sql), values)
                            .fetch_all(&mut *tx)
                            .await?,
                    );
                }
                tx.commit().await?;
                for row in &out {
                    flare_db::events::emit(#table, flare_db::MutationKind::Upserted, Some(row.#pk_ident.to_string()));
                }
                Ok(out)
            }

            /// `INSERT .. ON CONFLICT (conflict) DO UPDATE`. `conflict` must match a
            /// UNIQUE index. Duplicate conflict keys within one batch are a database error.
            pub async fn upsert_many_on<'e>(pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>, conflict: &[#field_ty], news: Vec<#new_ty>) -> flare_db::sqlx::Result<Vec<#out_ty>> {
                let target: Vec<&'static str> = conflict.iter().map(|f| f.as_str()).collect();
                Self::__upsert(pool, &target, news).await
            }

            pub async fn upsert_one_on<'e>(pool: impl flare_db::sqlx::Acquire<'e, Database = flare_db::Database>, conflict: &[#field_ty], new: #new_ty) -> flare_db::sqlx::Result<#out_ty> {
                Ok(Self::upsert_many_on(pool, conflict, vec![new]).await?.remove(0))
            }
            #unique_upserts

            #delete_methods
        }
    }
}
