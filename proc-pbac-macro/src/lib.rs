// `#[require_policies(policies = "...")]` — an attribute macro for
// declarative policy-based access control (PBAC).
// It wraps an `async` method so that, before the original body runs, it
// checks the caller's policies through a `PolicyGuard` implementation and
// bails out with `PolicyError::Forbidden` if the check fails. The generated
// code expects two things to be in scope: a `self` that implements
// `pbac_core::PolicyGuard`, and a `ctx: pbac_core::PolicyContext` argument.
//

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Expr, ExprLit, ItemFn, Lit, Meta};

#[proc_macro_attribute]
pub fn require_policies(attr: TokenStream, item: TokenStream) -> TokenStream {
    let policies_meta = parse_macro_input!(attr as Meta);
    let input_fn = parse_macro_input!(item as ItemFn);

    let policies_str = match extract_str_literal(policies_meta) {
        Ok(value) => value,
        Err(err) => return err.to_compile_error().into(),
    };

    let policies: Vec<String> = policies_str
        .split_whitespace()
        .map(str::to_string)
        .collect();

    let vis = &input_fn.vis;
    let asyncness = &input_fn.sig.asyncness;
    let fn_name = &input_fn.sig.ident;
    let fn_args = &input_fn.sig.inputs;
    let fn_return = &input_fn.sig.output;
    let fn_body = &input_fn.block;

    let guard_check = quote! {
        let __required_policies: &[&str] = &[#(#policies),*];
        if !self.check_policies(&ctx, __required_policies).await {
            return Err(pbac_core::PolicyError::Forbidden);
        }
    };

    let output = quote! {
        #vis #asyncness fn #fn_name(#fn_args) #fn_return {
            #guard_check
            #fn_body
        }
    };

    output.into()
}

/// Pulls the string out of a `name = "value"` attribute argument.
fn extract_str_literal(meta: Meta) -> syn::Result<String> {
    match meta {
        Meta::NameValue(name_value) => match name_value.value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(lit_str),
                ..
            }) => Ok(lit_str.value()),
            other => Err(syn::Error::new_spanned(other, "expected a string literal")),
        },
        other => Err(syn::Error::new_spanned(
            other,
            "expected `policies = \"...\"`",
        )),
    }
}
