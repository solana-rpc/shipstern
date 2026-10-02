use codama_nodes::CamelCaseString;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

///
/// Generate event support code: `resolve_event_default()`, `resolve_events_from_logs()`,
/// and `ProgramEventOutput` struct.
///
/// The event resolution functions are called by `InstructionParser` (when
/// `program-events` feature is active) to handle CPI and log-based events.
///
/// An event paired with the envelope resolved for it by the validated pass.
type EnvelopedEvent<'a> = (
    &'a codama_nodes::EventNode,
    Option<&'a crate::parse::CpiEventEnvelope>,
);

pub fn event_parser(
    _program_name_camel: &CamelCaseString,
    events: &[codama_nodes::EventNode],
    has_instructions: bool,
    envelope: Option<&crate::parse::ProgramEnvelope>,
) -> TokenStream {
    let wrapper_ident = format_ident!("Events");
    let ev_mod = format_ident!("event");

    // Per-event parse helper functions
    // Resolved once from the validated pass so no codegen site re-decides it.
    let event_envelopes: Vec<Option<&crate::parse::CpiEventEnvelope>> = envelope.map_or_else(
        || vec![None; events.len()],
        |program| program.resolve(events),
    );

    let helper_fns: Vec<TokenStream> = events
        .iter()
        .zip(&event_envelopes)
        .filter_map(|(ev, envelope)| single_event_helper_fn(ev, &wrapper_ident, &ev_mod, *envelope))
        .collect();

    // Discriminator match arms
    let mut groups: Vec<(
        super::instruction_parser::DiscriminatorKey,
        Vec<EnvelopedEvent<'_>>,
    )> = Vec::new();

    for (ev, envelope) in events.iter().zip(&event_envelopes) {
        if let Some(key) = super::instruction_parser::extract_event_discriminator_key(ev, *envelope)
        {
            if let Some(group) = groups.iter_mut().find(|(k, _)| k == &key) {
                group.1.push((ev, *envelope));
            } else {
                groups.push((key, vec![(ev, *envelope)]));
            }
        }
    }

    let match_arms: Vec<TokenStream> = groups
        .iter()
        .filter_map(|(_, evs)| {
            if evs.len() == 1 {
                single_event_match_arm(evs[0].0, &ev_mod, evs[0].1)
            } else {
                // Events shouldn't have discriminator collisions in practice.
                // If they do, emit a compile-time error directing the user to investigate.
                let names: Vec<_> = evs.iter().map(|(e, _)| e.name.to_string()).collect();
                let msg = format!(
                    "Event discriminator collision: [{}] share the same discriminator.",
                    names.join(", ")
                );

                Some(quote! { compile_error!(#msg); })
            }
        })
        .collect();

    let resolve_events_from_logs = quote! {
        ///
        /// Resolve events from this program's own `Program data:` log lines. Lines
        /// matching no discriminator are skipped, and nested CPI lines from other
        /// programs are ignored even when their discriminators collide.
        ///
        pub fn resolve_events_from_logs(
            logs: &[String],
        ) -> Vec<#wrapper_ident> {
            const PREFIX: &str = "Program data: ";
            const PROGRAM_PREFIX: &str = "Program ";

            fn invoked_program(line: &str) -> Option<&str> {
                let line = line.strip_prefix(PROGRAM_PREFIX)?;
                let (program, _) = line.split_once(" invoke [")?;
                Some(program)
            }

            fn closes_invocation(line: &str) -> bool {
                line.starts_with(PROGRAM_PREFIX)
                    && (line.ends_with(" success") || line.contains(" failed:"))
            }

            let target_program = ::shipstern_core::Pubkey::new(PROGRAM_ID).to_string();
            let mut invocation_stack = Vec::<String>::new();
            let mut events = Vec::new();

            for line in logs {
                if let Some(program) = invoked_program(line) {
                    invocation_stack.push(program.to_owned());
                    continue;
                }

                if closes_invocation(line) {
                    invocation_stack.pop();
                    continue;
                }

                let Some(encoded) = line.strip_prefix(PREFIX) else {
                    continue;
                };

                if invocation_stack.last().map(String::as_str) != Some(target_program.as_str()) {
                    continue;
                }

                let Some(decoded) = shipstern_parser::base64::Engine::decode(
                    &shipstern_parser::base64::engine::general_purpose::STANDARD,
                    encoded.trim(),
                ).ok() else {
                    continue;
                };

                if let Ok(event) = resolve_event_default(&[], &decoded) {
                    events.push(event);
                }
            }

            events
        }
    };

    let program_event_output = generate_program_event_output(has_instructions);

    quote! {
        #(#helper_fns)*

        ///
        /// Default event resolution using discriminator matching.
        ///
        pub fn resolve_event_default(
            accounts: &[::shipstern_core::Pubkey],
            data: &[u8],
        ) -> ParseResult<#wrapper_ident> {
            #(#match_arms)*

            Err(ParseError::Filtered)
        }

        #resolve_events_from_logs

        #program_event_output
    }
}

/// Generate the concrete `ProgramEventOutput` struct and optionally its
/// `prost::Message` impl (behind `proto` feature).
fn generate_program_event_output(has_instructions: bool) -> TokenStream {
    let instruction_field = if has_instructions {
        quote! {
            /// Parsed instruction (None if this was a CPI event or filtered).
            pub instruction: Option<Instructions>,
        }
    } else {
        quote! {}
    };

    let proto_impl = if cfg!(feature = "proto") {
        let encode_instruction = if has_instructions {
            quote! {
                if let Some(ref ix) = self.instruction {
                    ::prost::encoding::message::encode(1, ix, buf);
                }
            }
        } else {
            quote! {}
        };

        let encoded_len_instruction = if has_instructions {
            quote! {
                if let Some(ref ix) = self.instruction {
                    len += ::prost::encoding::message::encoded_len(1, ix);
                }
            }
        } else {
            quote! {}
        };

        let clear_instruction = if has_instructions {
            quote! { self.instruction = None; }
        } else {
            quote! {}
        };

        let default_instruction = if has_instructions {
            quote! { instruction: None, }
        } else {
            quote! {}
        };

        quote! {
            impl ::prost::Message for ProgramEventOutput {
                fn encode_raw(&self, buf: &mut impl ::prost::bytes::BufMut) {
                    #encode_instruction

                    for event in &self.program_events {
                        ::prost::encoding::message::encode(2, event, buf);
                    }
                }

                #[allow(deprecated)]
                fn merge_field(
                    &mut self,
                    _tag: u32,
                    _wire_type: ::prost::encoding::WireType,
                    _buf: &mut impl ::prost::bytes::Buf,
                    _ctx: ::prost::encoding::DecodeContext,
                ) -> Result<(), ::prost::DecodeError> {
                    // ProgramEventOutput is encode-only. Decoding from proto is not supported
                    // because the inner types (Instructions) do not implement Default.
                    Err(::prost::DecodeError::new(
                        "ProgramEventOutput does not support proto decoding",
                    ))
                }

                fn encoded_len(&self) -> usize {
                    let mut len = 0;

                    #encoded_len_instruction

                    for event in &self.program_events {
                        len += ::prost::encoding::message::encoded_len(2, event);
                    }

                    len
                }

                fn clear(&mut self) {
                    #clear_instruction
                    self.program_events.clear();
                }
            }

            impl Default for ProgramEventOutput {
                fn default() -> Self {
                    Self {
                        #default_instruction
                        program_events: Vec::new(),
                    }
                }
            }
        }
    } else {
        quote! {}
    };

    quote! {
        /// Combined output from instruction + event parsing.
        ///
        /// Generated per-program by the proc-macro with concrete `Instructions`
        /// and `Events` types.
        #[derive(Debug, PartialEq)]
        pub struct ProgramEventOutput {
            #instruction_field
            /// Events parsed from logs and/or CPI self-invocations.
            pub program_events: Vec<Events>,
        }

        #proto_impl
    }
}

fn single_event_helper_fn(
    event: &codama_nodes::EventNode,
    wrapper_ident: &syn::Ident,
    ev_mod: &syn::Ident,
    envelope: Option<&crate::parse::CpiEventEnvelope>,
) -> Option<TokenStream> {
    let ev_name_pascal = crate::utils::to_pascal_case(&event.name);
    let ev_name_snake = crate::utils::to_snake_case(&event.name);

    let variant_ident = format_ident!("{}", ev_name_pascal);
    let accounts_ident = format_ident!("{}Accounts", ev_name_pascal);
    let args_ident = format_ident!("{}Args", ev_name_pascal);
    let fn_ident = format_ident!("parse_event_{}", ev_name_snake);

    let struct_node = crate::intermediate_representation::helpers::unwrap_event_struct(&event.data);

    let has_args = !struct_node.fields.is_empty();

    let info = super::instruction_parser::extract_event_discriminator_info(
        event,
        &args_ident,
        has_args,
        ev_mod,
        envelope,
    )?;

    // Events have no accounts — only remaining_accounts (always empty vec)
    let accounts_value = quote! {
        event::#accounts_ident {
            remaining_accounts: vec![],
        }
    };

    let args_field = match info.args_expr {
        Some(expr) => quote! { args: #expr, },
        // Fieldless event: Event variant still declares `args`, initialize with empty struct.
        None => quote! { args: event::#args_ident {}, },
    };

    Some(quote! {
        pub fn #fn_ident(
            accounts: &[::shipstern_core::Pubkey],
            data: &[u8],
        ) -> ParseResult<#wrapper_ident> {
            Ok(#wrapper_ident {
                event: event::Event::#variant_ident {
                    accounts: #accounts_value,
                    #args_field
                },
            })
        }
    })
}

fn single_event_match_arm(
    event: &codama_nodes::EventNode,
    ev_mod: &syn::Ident,
    envelope: Option<&crate::parse::CpiEventEnvelope>,
) -> Option<TokenStream> {
    let ev_name_snake = crate::utils::to_snake_case(&event.name);
    let fn_ident = format_ident!("parse_event_{}", ev_name_snake);
    let args_ident = format_ident!("{}Args", crate::utils::to_pascal_case(&event.name));

    let struct_node = crate::intermediate_representation::helpers::unwrap_event_struct(&event.data);

    let has_args = !struct_node.fields.is_empty();

    let info = super::instruction_parser::extract_event_discriminator_info(
        event,
        &args_ident,
        has_args,
        ev_mod,
        envelope,
    )?;

    let check = info.check;

    Some(quote! {
        if {
            #check
        } {
            return #fn_ident(accounts, data);
        }
    })
}
