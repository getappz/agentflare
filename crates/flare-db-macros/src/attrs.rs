//! Parsing of `#[crud(...)]` on the struct and on its fields.

use syn::{DeriveInput, Expr, Field, Ident, LitStr, Type};

pub struct CrudConfig {
    pub table: LitStr,
    pub pk: Ident,
    pub soft_delete: bool,
    /// `#[crud(validate)]`: call `flare_db::Validate` on New/Patch before writes.
    pub validate: bool,
    /// `#[crud(unique(a, b))]`: default conflict target for `upsert_one`/`upsert_many`.
    pub unique: Vec<Ident>,
}

pub fn parse_config(input: &DeriveInput) -> syn::Result<CrudConfig> {
    let mut table: Option<LitStr> = None;
    let mut pk: Option<Ident> = None;
    let mut soft_delete = false;
    let mut validate = false;
    let mut unique = Vec::new();

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
            table = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("pk") {
            let lit: LitStr = meta.value()?.parse()?;
            pk = Some(Ident::new(&lit.value(), lit.span()));
        } else if meta.path.is_ident("soft_delete") {
            soft_delete = true;
        } else if meta.path.is_ident("validate") {
            validate = true;
        } else if meta.path.is_ident("unique") {
            meta.parse_nested_meta(|col| {
                unique.push(
                    col.path
                        .get_ident()
                        .cloned()
                        .ok_or_else(|| col.error("expected a field name"))?,
                );
                Ok(())
            })?;
        } else {
            return Err(meta.error(
                "unrecognized #[crud(...)] key — expected `table`, `pk`, `soft_delete`, \
                 `validate` or `unique(...)`",
            ));
        }
        Ok(())
    })?;

    Ok(CrudConfig {
        table: table.ok_or_else(|| {
            syn::Error::new_spanned(attr, "#[crud(...)] is missing required `table = \"...\"`")
        })?,
        pk: pk.ok_or_else(|| {
            syn::Error::new_spanned(attr, "#[crud(...)] is missing required `pk = \"...\"`")
        })?,
        soft_delete,
        validate,
        unique,
    })
}

/// Field-level policy, written as `#[crud(...)]` on a struct field.
#[derive(Default)]
pub struct FieldAttrs {
    /// Readable; never written through New/Patch (DB default / trigger owns it).
    pub readonly: bool,
    /// Settable on insert, absent from Patch and from upsert updates.
    pub immutable: bool,
    /// Never selected or returned; generated output type lacks it.
    pub hidden: bool,
    /// Not a column: skipped in all SQL (needs `Default` for the output type).
    pub computed: bool,
    pub created_at: bool,
    pub updated_at: bool,
    pub searchable: bool,
    /// Prefix for a generated `prefix_<ulid>` id when the caller passes `None`.
    pub id_prefix: Option<LitStr>,
    /// Insert-time default applied when the caller passes `None`.
    pub default: Option<Expr>,
    pub enum_values: Vec<LitStr>,
}

pub struct FieldInfo {
    pub ident: Ident,
    pub ty: Type,
    pub attrs: FieldAttrs,
}

pub fn parse_field(f: &Field) -> syn::Result<FieldInfo> {
    let mut a = FieldAttrs::default();
    for attr in f.attrs.iter().filter(|a| a.path().is_ident("crud")) {
        attr.parse_nested_meta(|meta| {
            let p = &meta.path;
            if p.is_ident("readonly") {
                a.readonly = true;
            } else if p.is_ident("immutable") {
                a.immutable = true;
            } else if p.is_ident("hidden") {
                a.hidden = true;
            } else if p.is_ident("computed") {
                a.computed = true;
            } else if p.is_ident("created_at") {
                a.created_at = true;
            } else if p.is_ident("updated_at") {
                a.updated_at = true;
            } else if p.is_ident("searchable") {
                a.searchable = true;
            } else if p.is_ident("id") {
                meta.parse_nested_meta(|inner| {
                    if inner.path.is_ident("prefix") {
                        a.id_prefix = Some(inner.value()?.parse()?);
                        Ok(())
                    } else {
                        Err(inner.error("expected `prefix = \"...\"`"))
                    }
                })?;
            } else if p.is_ident("default") {
                a.default = Some(meta.value()?.parse()?);
            } else if p.is_ident("enum_values") {
                let content;
                syn::parenthesized!(content in meta.input);
                a.enum_values = content
                    .parse_terminated(|i| i.parse::<LitStr>(), syn::Token![,])?
                    .into_iter()
                    .collect();
            } else {
                return Err(meta.error(
                    "unrecognized field `#[crud(...)]` key — expected `readonly`, `immutable`, \
                     `hidden`, `computed`, `created_at`, `updated_at`, `searchable`, \
                     `id(prefix = \"..\")`, `default = <expr>` or `enum_values(..)`",
                ));
            }
            Ok(())
        })?;
    }
    let ident = f.ident.clone().expect("named field");

    let err = |msg: &str| Err(syn::Error::new_spanned(&ident, msg));
    if a.computed
        && (a.readonly
            || a.immutable
            || a.hidden
            || a.created_at
            || a.updated_at
            || a.searchable
            || a.id_prefix.is_some()
            || a.default.is_some()
            || !a.enum_values.is_empty())
    {
        return err("`computed` fields are not columns and cannot carry other crud policies");
    }
    if a.readonly && a.immutable {
        return err("`readonly` already implies never-written; drop `immutable`");
    }
    if (a.created_at || a.updated_at)
        && (a.id_prefix.is_some() || a.default.is_some() || a.immutable || a.hidden)
    {
        return err(
            "`created_at`/`updated_at` cannot be combined with id/default/immutable/hidden",
        );
    }
    if a.id_prefix.is_some() && a.default.is_some() {
        return err("`id(..)` and `default = ..` are mutually exclusive");
    }

    Ok(FieldInfo {
        ident,
        ty: f.ty.clone(),
        attrs: a,
    })
}
