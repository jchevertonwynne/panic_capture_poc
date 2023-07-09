use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::quote;
use syn::ItemFn;

#[proc_macro_attribute]
pub fn capture_panics(_args: TokenStream, items: TokenStream) -> TokenStream {
    let mut f: ItemFn = syn::parse(items).unwrap();
    let new_ident = Ident::new("__inner", f.sig.ident.span());
    f.sig.ident = new_ident;

    #[allow(clippy::redundant_clone)]
    let return_type = f.sig.output.clone();

    quote!{
        fn main() #return_type {
            #f

            match ::std::panic::catch_unwind(std::panic::AssertUnwindSafe(__inner)) {
                Ok(res) => res,
                Err(err) => {
                    let panics = ::panic_capture_lib::increment_counter();
                    ::tracing::error!("this is from my macro panic capture. total panics = {panics}");
                    ::std::panic::resume_unwind(err)
                }
            }
        }
    }.into()
}
