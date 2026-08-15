// Copyright 2025 FastLabs Developers
// Licensed under the Apache License, Version 2.0.

//! Procedural macro implementation for the `stacksafe` crate.
//!
//! This local compatibility patch preserves the 0.1.4 API while replacing
//! the unmaintained `proc-macro-error2` dependency with native `syn` errors.

use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::{parse_macro_input, parse_quote, ItemFn, Path, ReturnType, Type};

#[proc_macro_attribute]
pub fn stacksafe(args: TokenStream, item: TokenStream) -> TokenStream {
    let mut crate_path: Option<Path> = None;

    let arg_parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("crate") {
            crate_path = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error(format!(
                "unknown attribute parameter `{}`",
                meta.path
                    .get_ident()
                    .map_or("unknown".to_string(), |ident| ident.to_string())
            )))
        }
    });
    parse_macro_input!(args with arg_parser);

    let mut item_fn: ItemFn = match syn::parse(item) {
        Ok(item_fn) => item_fn,
        Err(error) => {
            return syn::Error::new(
                error.span(),
                "#[stacksafe] can only be applied to functions",
            )
            .into_compile_error()
            .into();
        }
    };

    if let Some(asyncness) = &item_fn.sig.asyncness {
        return syn::Error::new_spanned(asyncness, "#[stacksafe] does not support async functions")
            .into_compile_error()
            .into();
    }

    let block = item_fn.block;
    let ret = match &item_fn.sig.output {
        ReturnType::Type(_, ty) if matches!(**ty, Type::ImplTrait(_)) => ReturnType::Default,
        _ => item_fn.sig.output.clone(),
    };

    let stacksafe_crate = crate_path.unwrap_or_else(|| parse_quote!(::stacksafe));
    let wrapped_block = quote! {
        {
            #stacksafe_crate::internal::stacker::maybe_grow(
                #stacksafe_crate::get_minimum_stack_size(),
                #stacksafe_crate::get_stack_allocation_size(),
                #stacksafe_crate::internal::with_protected(move || #ret { #block })
            )
        }
    };
    let wrapped_block = match syn::parse2(wrapped_block) {
        Ok(block) => block,
        Err(error) => return error.into_compile_error().into(),
    };

    item_fn.block = Box::new(wrapped_block);
    item_fn.into_token_stream().into()
}
