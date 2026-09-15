// `#[derive(GeoFfiType)]` — generates the `From`/`Into` glue between an
// idiomatic Rust struct and a native geometry type exposed across an FFI
// boundary (e.g. a `cxx` bridge's `shared struct`), so callers don't have
// to hand-write a conversion impl for every field every time the native
// layout changes.

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Ident, LitStr, Path};

#[proc_macro_derive(GeoFfiType, attributes(geo_ffi))]
pub fn derive_geo_ffi_type(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let target_path = match extract_target(&input.attrs) {
        Ok(path) => path,
        Err(err) => return err.to_compile_error().into(),
    };

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new_spanned(
                    &input.ident,
                    "GeoFfiType only supports structs with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(&input.ident, "GeoFfiType only supports structs")
                .to_compile_error()
                .into();
        }
    };

    let struct_name = &input.ident;
    let mut to_target = Vec::new();
    let mut from_target = Vec::new();

    for field in fields {
        let rust_ident = field.ident.clone().expect("checked above: named fields");

        let target_ident = match extract_rename(&field.attrs) {
            Ok(Some(renamed)) => renamed,
            Ok(None) => rust_ident.clone(),
            Err(err) => return err.to_compile_error().into(),
        };

        to_target.push(quote! { #target_ident: value.#rust_ident });
        from_target.push(quote! { #rust_ident: value.#target_ident });
    }

    let expanded = quote! {
        impl ::core::convert::From<#struct_name> for #target_path {
            fn from(value: #struct_name) -> Self {
                #target_path { #(#to_target),* }
            }
        }

        impl ::core::convert::From<#target_path> for #struct_name {
            fn from(value: #target_path) -> Self {
                #struct_name { #(#from_target),* }
            }
        }
    };

    expanded.into()
}

/// Reads `#[geo_ffi(target = "path::to::Type")]` off the struct.
fn extract_target(attrs: &[syn::Attribute]) -> syn::Result<Path> {
    for attr in attrs {
        if !attr.path().is_ident("geo_ffi") {
            continue;
        }

        let mut target = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("target") {
                let lit: LitStr = meta.value()?.parse()?;
                target = Some(lit.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error("unsupported `geo_ffi` key, expected `target`"))
            }
        })?;

        if let Some(target) = target {
            return Ok(target);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "GeoFfiType requires #[geo_ffi(target = \"path::to::Type\")]",
    ))
}

/// Reads an optional `#[geo_ffi(rename = "native_name")]` off a field.
fn extract_rename(attrs: &[syn::Attribute]) -> syn::Result<Option<Ident>> {
    for attr in attrs {
        if !attr.path().is_ident("geo_ffi") {
            continue;
        }

        let mut renamed = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let lit: LitStr = meta.value()?.parse()?;
                renamed = Some(Ident::new(&lit.value(), lit.span()));
                Ok(())
            } else {
                Err(meta.error("unsupported `geo_ffi` key, expected `rename`"))
            }
        })?;

        if renamed.is_some() {
            return Ok(renamed);
        }
    }

    Ok(None)
}
