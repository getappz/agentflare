//! The resolved entity model: which fields land in which generated type / SQL list.
//! Every field-policy decision lives here so New, Patch, Filter, SELECT lists and
//! upsert updates cannot drift apart.

use crate::attrs::{CrudConfig, FieldInfo};
use proc_macro2::Span;
use syn::{Ident, LitStr, Type};

pub struct Model {
    pub name: Ident,
    pub cfg: CrudConfig,
    pub fields: Vec<FieldInfo>,
    pub pk: usize,
    /// Inner type of `deleted_at: Option<T>` under `soft_delete`.
    pub deleted_inner: Option<Type>,
}

pub fn unwrap_option(ty: &Type) -> Option<&Type> {
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

pub fn col_lit(f: &FieldInfo) -> LitStr {
    LitStr::new(&f.ident.to_string(), f.ident.span())
}

pub fn pascal(ident: &Ident) -> Ident {
    let s: String = ident
        .to_string()
        .trim_start_matches("r#")
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|h| h.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    Ident::new(&s, Span::call_site())
}

impl Model {
    pub fn new(
        name: Ident,
        cfg: CrudConfig,
        fields: Vec<FieldInfo>,
        span_item: &syn::DeriveInput,
    ) -> syn::Result<Self> {
        let Some(pk) = fields.iter().position(|f| f.ident == cfg.pk) else {
            return Err(syn::Error::new_spanned(
                span_item,
                format!(
                    "#[crud(pk = \"{}\")] does not match any field on this struct",
                    cfg.pk
                ),
            ));
        };
        let pf = &fields[pk];
        if pf.attrs.hidden || pf.attrs.computed {
            return Err(syn::Error::new_spanned(
                &pf.ident,
                "the primary key cannot be `hidden` or `computed`",
            ));
        }
        for u in &cfg.unique {
            if !fields.iter().any(|f| f.ident == *u && !f.attrs.computed) {
                return Err(syn::Error::new_spanned(
                    u,
                    "`unique(..)` names a field that is not a column of this struct",
                ));
            }
        }
        let deleted_inner = if cfg.soft_delete {
            let Some(f) = fields.iter().find(|f| f.ident == "deleted_at") else {
                return Err(syn::Error::new_spanned(
                    span_item,
                    "#[crud(soft_delete)] requires a `deleted_at: Option<T>` field",
                ));
            };
            let Some(inner) = unwrap_option(&f.ty) else {
                return Err(syn::Error::new_spanned(
                    &f.ty,
                    "#[crud(soft_delete)] requires `deleted_at` to be `Option<T>`",
                ));
            };
            Some(inner.clone())
        } else {
            None
        };
        Ok(Self {
            name,
            cfg,
            fields,
            pk,
            deleted_inner,
        })
    }

    pub fn pk(&self) -> &FieldInfo {
        &self.fields[self.pk]
    }

    fn is_pk(&self, f: &FieldInfo) -> bool {
        f.ident == self.cfg.pk
    }

    fn is_soft_deleted_at(&self, f: &FieldInfo) -> bool {
        self.cfg.soft_delete && f.ident == "deleted_at"
    }

    fn is_auto_ts(f: &FieldInfo) -> bool {
        f.attrs.created_at || f.attrs.updated_at
    }

    /// Real columns (everything but `computed`). Filterable.
    pub fn columns(&self) -> Vec<&FieldInfo> {
        self.fields.iter().filter(|f| !f.attrs.computed).collect()
    }

    /// Columns that are ever selected / returned (`hidden` and `computed` excluded).
    pub fn visible(&self) -> Vec<&FieldInfo> {
        self.fields
            .iter()
            .filter(|f| !f.attrs.computed && !f.attrs.hidden)
            .collect()
    }

    /// `hidden`/`computed` fields force a separate generated output type.
    pub fn needs_public(&self) -> bool {
        self.fields
            .iter()
            .any(|f| f.attrs.hidden || f.attrs.computed)
    }

    /// Fields the INSERT writes: caller-supplied ones plus auto timestamps. The pk is
    /// caller-supplied only when it carries `id(prefix)`; `deleted_at` (soft delete)
    /// and `readonly` fields are never inserted.
    pub fn insert_fields(&self) -> Vec<&FieldInfo> {
        self.fields
            .iter()
            .filter(|f| {
                !f.attrs.computed
                    && !f.attrs.readonly
                    && !self.is_soft_deleted_at(f)
                    && (!self.is_pk(f) || f.attrs.id_prefix.is_some())
            })
            .collect()
    }

    /// Fields of `{E}New` (insert fields minus auto timestamps).
    pub fn new_fields(&self) -> Vec<&FieldInfo> {
        self.insert_fields()
            .into_iter()
            .filter(|f| !Self::is_auto_ts(f))
            .collect()
    }

    /// Fields of `{E}Patch`.
    pub fn patch_fields(&self) -> Vec<&FieldInfo> {
        self.fields
            .iter()
            .filter(|f| {
                !f.attrs.computed
                    && !f.attrs.readonly
                    && !f.attrs.immutable
                    && !Self::is_auto_ts(f)
                    && !self.is_soft_deleted_at(f)
                    && !self.is_pk(f)
            })
            .collect()
    }

    /// Columns an upsert's `DO UPDATE` may overwrite: patchable fields except those
    /// with an insert-time `default` (an unset default must not clobber the stored
    /// value), plus `updated_at`.
    pub fn upsert_update_cols(&self) -> Vec<LitStr> {
        self.fields
            .iter()
            .filter(|f| {
                f.attrs.updated_at
                    || (self.patch_fields().iter().any(|p| p.ident == f.ident)
                        && f.attrs.default.is_none())
            })
            .map(col_lit)
            .collect()
    }

    pub fn updated_at(&self) -> Option<LitStr> {
        self.fields.iter().find(|f| f.attrs.updated_at).map(col_lit)
    }

    pub fn searchable(&self) -> Vec<LitStr> {
        self.fields
            .iter()
            .filter(|f| f.attrs.searchable && !f.attrs.computed && !f.attrs.hidden)
            .map(col_lit)
            .collect()
    }
}
