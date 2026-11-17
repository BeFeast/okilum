//! `#[gpui::test]` for okilum-shell; see okilum-gpui for why (#1669).
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, parse_quote, FnArg, ItemFn, Pat, Type};

/// Expands to gpui-pre's `#[gpui::test]` with the same arguments, after
/// giving each `&mut TestAppContext` argument a guard that runs
/// `TestAppContext::quit` when the test body returns.
#[proc_macro_attribute]
pub fn test(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = proc_macro2::TokenStream::from(args);
    let mut function = parse_macro_input!(item as ItemFn);
    let mut guards = Vec::new();
    for (index, input) in function.sig.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = input else {
            continue;
        };
        if !is_test_app_context(&argument.ty) {
            continue;
        }
        let context = match &*argument.pat {
            Pat::Ident(pattern) => pattern.ident.clone(),
            _ => {
                let context = format_ident!("__okilum_test_cx_{index}");
                *argument.pat = parse_quote!(#context);
                context
            }
        };
        let guard = format_ident!("__okilum_quit_on_test_end_{index}");
        guards.push(quote! {
            let #guard = ::gpui::__test_support::QuitOnTestEnd::new(&*#context);
        });
    }
    if !guards.is_empty() {
        // Declared first, so dropped after every other local of the body.
        let body = &function.block;
        function.block = parse_quote!({ #(#guards)* #body });
    }
    let upstream = if args.is_empty() {
        quote!(#[::gpui::upstream_test])
    } else {
        quote!(#[::gpui::upstream_test(#args)])
    };
    quote!(#upstream #function).into()
}

/// The argument shapes gpui-pre's macro builds a `TestAppContext` for.
fn is_test_app_context(ty: &Type) -> bool {
    let Type::Reference(reference) = ty else {
        return false;
    };
    let Type::Path(path) = &*reference.elem else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "TestAppContext")
}
