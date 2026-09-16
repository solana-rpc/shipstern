use base64::{engine::general_purpose::STANDARD, Engine};
use codama_nodes::{
    CamelCaseString, DiscriminatorNode, NestedTypeNode, Number, TypeNode, ValueNode,
};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

fn decode_discriminator_bytes(bytes: &codama_nodes::BytesValueNode) -> Vec<u8> {
    match bytes.encoding {
        codama_nodes::BytesEncoding::Base16 => {
            let padded = crate::utils::pad_hex(&bytes.data);
            hex::decode(&padded).expect("decode base16 discriminator")
        },
        codama_nodes::BytesEncoding::Base58 => bs58::decode(&bytes.data)
            .into_vec()
            .expect("decode base58 discriminator"),
        codama_nodes::BytesEncoding::Base64 => STANDARD
            .decode(&bytes.data)
            .expect("decode base64 discriminator"),
        codama_nodes::BytesEncoding::Utf8 => bytes.data.as_bytes().to_vec(),
    }
}

///
/// Build the discriminator constants for a generated account type.
///
/// The pair is a memcmp predicate: `DISCRIMINATOR` is exactly what the parser
/// compares at `DISCRIMINATOR_OFFSET`. It is not a payload boundary. A numeric
/// discriminator is re-read as the first field of the account body, so callers
/// must decode through `try_unpack` rather than slicing past it.
///
/// Returns `None` for an empty discriminator, which would match every account.
///
fn discriminator_consts(
    ident: &proc_macro2::Ident,
    bytes: &[u8],
    offset: &proc_macro2::Literal,
) -> Option<TokenStream> {
    if bytes.is_empty() {
        return None;
    }

    Some(quote! {
        impl #ident {
            /// Discriminator bytes that identify this account type on the wire.
            ///
            /// Pairs with [`Self::DISCRIMINATOR_OFFSET`] as a memcmp predicate:
            /// these bytes appear at that offset in the raw account data. It is
            /// not a payload boundary. Decode with `try_unpack`.
            pub const DISCRIMINATOR: &'static [u8] = &[#(#bytes),*];

            /// Byte offset at which [`Self::DISCRIMINATOR`] begins in the account data.
            pub const DISCRIMINATOR_OFFSET: usize = #offset;
        }
    })
}

///
/// Build the *account parser* for a program.
///
/// Generates a wrapper struct with a non-Option oneof field and a manual
/// `prost::Message` impl when `proto` is enabled. prost derives require
/// `Option` for oneof fields, so we bypass them by implementing `Message`
/// manually.
///
/// Example output:
///
/// ```rust, ignore
/// // --- wrapper struct + enum (identical shape for proto and non-proto) ---
///
/// #[derive(Clone, PartialEq)]          // + Debug when non-proto
/// pub struct {ProgramName}Account {
///     pub account: account::Account,
/// }
///
/// pub mod account {
///     #[derive(Clone, PartialEq, ::prost::Oneof)]  // or Clone, Debug, PartialEq
///     pub enum Account {
///         AccountA(super::AccountA),
///         AccountB(super::AccountB),
///         // ...
///     }
/// }
///
/// // --- proto only: manual Debug + prost::Message impls ---
///
/// impl Debug for {ProgramName}Account { ... }
/// impl prost::Message for {ProgramName}Account { ... }
///
/// // --- try_unpack: discriminator-based deserialization ---
///
/// impl {ProgramName}Account {
///     pub fn try_unpack(data: &[u8]) -> ParseResult<Self> { ... }
/// }
///
/// // --- AccountParser + Parser impl + ProgramParser impl ---
///
/// pub struct AccountParser;
/// impl Parser for AccountParser { ... }
/// impl ProgramParser for AccountParser { ... }
/// ```
///
pub fn account_parser(
    program_name_camel: &CamelCaseString,
    accounts: &[codama_nodes::AccountNode],
) -> TokenStream {
    let program_name = crate::utils::to_pascal_case(program_name_camel);

    // Detect collision: if any account is named `{program_name}Account` (e.g. program
    // "margin" with account "marginAccount"), the wrapper would shadow the data struct.
    // Fall back to `{program_name}AccountOutput` in that case.
    let wrapper_name = format!("{}Account", program_name);
    let wrapper_clashes = accounts
        .iter()
        .any(|a| crate::utils::to_pascal_case(&a.name) == wrapper_name);

    let account_struct_ident = if wrapper_clashes {
        format_ident!("{}AccountOutput", program_name)
    } else {
        format_ident!("{}Account", program_name)
    };

    let account_mod_ident = format_ident!("account");

    let parser_id = format!("{}::AccountParser", program_name);

    let parser_error_msg = format!("Unknown account for program {}", program_name);

    let oneof_variants = accounts.iter().enumerate().map(|(i, account)| {
        let tag = (i + 1) as u32;
        let account_ident = format_ident!("{}", crate::utils::to_pascal_case(&account.name));

        if cfg!(feature = "proto") {
            quote! {
                #[prost(message, tag = #tag)]
                #account_ident(super::#account_ident)
            }
        } else {
            quote! {
                #account_ident(super::#account_ident)
            }
        }
    });

    let account_entries = accounts.iter().filter_map(|account| {
        let discriminator = match account.discriminators.first() {
            Some(d) => d,
            None => {
                return None;
            }
        };

        let account_ident = format_ident!("{}", crate::utils::to_pascal_case(&account.name));

        Some(match discriminator {
            // Handle constant discriminators.
            DiscriminatorNode::Constant(node) => {
                let offset_at = crate::utils::as_index(node.offset);
                let offset = crate::utils::unsuffixed(offset_at as u64);

                match node.constant.value.as_ref() {
                    // Preserve numeric discriminator behavior for layouts that include the
                    // discriminator in the account struct (for example SPL Governance).
                    ValueNode::Number(nn) => {
                        let Number::UnsignedInteger(value) = nn.number else {
                            return None;
                        };

                        let value_u8 = value as u8;

                        let arm = quote! {
                            if let Some(discriminator) = data.get(#offset) {
                                if *discriminator == #value_u8 {
                                    match <#account_ident as ::borsh::BorshDeserialize>::deserialize(&mut &data[..]) {
                                        Ok(parsed) => {
                                            return Ok(#account_struct_ident {
                                                account: #account_mod_ident::Account::#account_ident(parsed),
                                            });
                                        }
                                        Err(e) => return Err(ParseError::Other(e.into())),
                                    }
                                }
                            }
                        };

                        (arm, discriminator_consts(&account_ident, &[value_u8], &offset))
                    },

                    // Byte discriminators are a prefix and are not part of the account struct.
                    ValueNode::Bytes(bytes) => {
                        let discriminator = decode_discriminator_bytes(bytes);
                        let end = crate::utils::unsuffixed((offset_at + discriminator.len()) as u64);

                        let arm = quote! {
                            if let Some(slice) = data.get(#offset..#end) {
                                if slice == &[#(#discriminator),*] {
                                    match <#account_ident as ::borsh::BorshDeserialize>::deserialize(&mut &data[#end..]) {
                                        Ok(parsed) => {
                                            return Ok(#account_struct_ident {
                                                account: #account_mod_ident::Account::#account_ident(parsed),
                                            });
                                        }
                                        Err(e) => return Err(ParseError::Other(e.into())),
                                    }
                                }
                            }
                        };

                        (arm, discriminator_consts(&account_ident, &discriminator, &offset))
                    },

                    _ => return None,
                }
            },

            // Handle multi-byte discriminators (like Anchor's 8 byte discriminators)
            DiscriminatorNode::Field(node) => {
                let offset_at = crate::utils::as_index(node.offset);
                let offset = crate::utils::unsuffixed(offset_at as u64);

                // Skip if not a struct
                let NestedTypeNode::Value(struct_node) = &account.data else {
                    return None;
                };

                // Find the discriminator field by name
                let field = match struct_node.fields.iter().find(|f| f.name == node.name) {
                    Some(f) => f,
                    None => {
                        return None;
                    }
                };

                // Skip if discriminator field isn't fixed-size bytes
                let TypeNode::FixedSize(fixed_size_node) = &*field.r#type else {
                    return None;
                };

                let size = fixed_size_node.size;

                // Skip if no default value
                let default_value = match field.default_value.as_ref() {
                    Some(v) => v,
                    None => {
                        return None;
                    }
                };

                // Skip if default value isn't bytes
                let ValueNode::Bytes(bytes) = default_value else {
                    return None;
                };

                // Decode expected discriminator bytes
                let discriminator = decode_discriminator_bytes(bytes);

                let end = crate::utils::unsuffixed((offset_at + size) as u64);

                // Empty discriminator (size 0) matches any data — skip the
                // byte comparison and match unconditionally on data length.
                let disc_check = if discriminator.is_empty() {
                    quote! { true }
                } else {
                    quote! { slice == &[#(#discriminator),*] }
                };

                let arm = quote! {
                    if let Some(slice) = data.get(#offset..#end) {
                        if #disc_check {
                            match <#account_ident as ::borsh::BorshDeserialize>::deserialize(&mut &data[#end..]) {
                                Ok(parsed) => {
                                    return Ok(#account_struct_ident {
                                        account: #account_mod_ident::Account::#account_ident(parsed),
                                    });
                                }
                                Err(e) => return Err(ParseError::Other(e.into())),
                            }
                        }
                    }
                };

                // The parser compares a `size`-wide slice against the decoded
                // default bytes, so a length mismatch can never match. Expose no
                // constant for a discriminator the parser cannot honor.
                let disc_const = if discriminator.len() == size {
                    discriminator_consts(&account_ident, &discriminator, &offset)
                } else {
                    None
                };

                (arm, disc_const)
            },

            // Handle accounts based on size only (e.g the account is 558 Bytes long)
            DiscriminatorNode::Size(node) => {
                let size = crate::utils::unsuffixed(node.size);

                let arm = quote! {
                    if data.len() == #size {
                        match <#account_ident as ::borsh::BorshDeserialize>::deserialize(&mut &data[..]) {
                            Ok(parsed) => {
                                return Ok(#account_struct_ident {
                                    account: #account_mod_ident::Account::#account_ident(parsed),
                                });
                            }
                            Err(e) => return Err(ParseError::Other(e.into())),
                        }
                    }
                };

                (arm, None)
            },
        })
    });

    let (account_matches, account_disc_consts): (Vec<TokenStream>, Vec<Option<TokenStream>>) =
        account_entries.unzip();

    let account_disc_consts = account_disc_consts.into_iter().flatten();

    let (struct_and_mod, proto_impls) = if accounts.is_empty() {
        let empty_struct = if cfg!(feature = "proto") {
            quote! {
                /// Wrapper struct for program accounts (no accounts defined).
                #[derive(Clone, PartialEq, ::prost::Message)]
                pub struct #account_struct_ident {}
            }
        } else {
            quote! {
                /// Wrapper struct for program accounts (no accounts defined).
                #[derive(Clone, Debug, PartialEq)]
                pub struct #account_struct_ident {}
            }
        };

        (empty_struct, quote! {})
    } else {
        let enum_derive = if cfg!(feature = "proto") {
            quote! { #[derive(Clone, PartialEq, ::prost::Oneof)] }
        } else {
            quote! { #[derive(Clone, Debug, PartialEq)] }
        };

        let debug_derive = if cfg!(feature = "proto") {
            quote! {}
        } else {
            quote! { Debug, }
        };

        let s = quote! {
            /// Wrapper struct for program accounts.
            #[derive(Clone, #debug_derive PartialEq)]
            pub struct #account_struct_ident {
                pub account: #account_mod_ident::Account,
            }

            pub mod #account_mod_ident {
                #enum_derive
                pub enum Account {
                    #(#oneof_variants),*
                }
            }
        };

        let p = if cfg!(feature = "proto") {
            let account_field = format_ident!("account");
            let account_enum = format_ident!("Account");

            super::manual_prost::manual_prost_message_impl(
                &account_struct_ident,
                &account_field,
                &account_mod_ident,
                &account_enum,
            )
        } else {
            quote! {}
        };

        (s, p)
    };

    quote! {
        #struct_and_mod

        #(#account_disc_consts)*

        #proto_impls

        impl #account_struct_ident {
            pub fn try_unpack(data: &[u8]) -> ParseResult<Self> {
                Self::try_unpack_inner(data)
            }

            fn try_unpack_inner(data: &[u8]) -> ParseResult<Self> {
                #(#account_matches)*

                Err(ParseError::from(#parser_error_msg.to_owned()))
            }
        }

        #[derive(Debug, Copy, Clone)]
        pub struct AccountParser;

        impl Parser for AccountParser {
            type Input = AccountUpdate;
            type Output = #account_struct_ident;

            fn id(&self) -> std::borrow::Cow<'static, str> {
                #parser_id.into()
            }

            fn prefilter(&self) -> Prefilter {
                Prefilter::builder()
                    .account_owners([PROGRAM_ID])
                    .build()
                    .unwrap()
            }

            async fn parse(&self, acct: &AccountUpdate) -> ParseResult<Self::Output> {
                let inner = acct
                    .account
                    .as_ref()
                    .ok_or_else(|| ParseError::from("Unable to unwrap account ref".to_owned()))?;

                if inner.owner != PROGRAM_ID {
                    return Err(ParseError::Filtered);
                }

                #account_struct_ident::try_unpack_inner(&inner.data)
            }
        }

        // Implement the trait for Mock
        impl ::shipstern_core::ProgramParser for AccountParser {
            #[inline]
            fn program_id(&self) -> shipstern_core::Pubkey {
                shipstern_core::Pubkey::new(PROGRAM_ID)
            }
        }
    }
}
