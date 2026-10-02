//! Derives `Crud` for a `#[derive(sqlx::FromRow, flare_db::Crud)]` struct: generates
//! `{Entity}New`/`Patch`/`Filter`/`Field`/`Partial` (and `Public` when the entity has
//! `hidden`/`computed` fields) plus an `impl` block with reads, writes, upsert and
//! (soft) delete. See flare-db's crate docs for the design rationale.
//!
//! Layout: `attrs` parses `#[crud(..)]`, `model` decides which field lands where,
//! `types` and `methods` emit code from that model.

mod attrs;
mod methods;
mod model;
mod types;

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

#[proc_macro_derive(Crud, attributes(crud))]
pub fn derive_crud(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let cfg = attrs::parse_config(input)?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(Crud)] only supports structs",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(Crud)] requires named fields",
        ));
    };
    let fields = named
        .named
        .iter()
        .map(attrs::parse_field)
        .collect::<syn::Result<Vec<_>>>()?;
    let model = model::Model::new(input.ident.clone(), cfg, fields, input)?;
    let names = types::Names::new(&model);
    let types = types::gen_types(&model, &names);
    let imp = methods::gen_impl(&model, &names);
    Ok(quote! { #types #imp })
}
