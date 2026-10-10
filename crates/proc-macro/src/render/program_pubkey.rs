use proc_macro2::TokenStream;
use quote::quote;

pub fn program_pubkey(pubkey: &str) -> TokenStream {
    let decoded = match bs58::decode(pubkey).into_vec() {
        Ok(bytes) => bytes,
        Err(_) => {
            return quote! { compile_error!(concat!("Invalid base58 pubkey: ", #pubkey)) };
        },
    };

    if decoded.len() != 32 {
        let len = decoded.len();

        return quote! {
            compile_error!(concat!(
                "Invalid pubkey length: expected 32 bytes, got ",
                stringify!(#len),
                " bytes for pubkey: ",
                #pubkey
            ))
        };
    }

    let bytes = decoded.iter().map(|b| quote!(#b));

    quote! {
        [#(#bytes),*]
    }
}
