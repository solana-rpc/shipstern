//! Expansion-time checks that turn a broken IDL into a `compile_error!` naming the
//! offending entry, instead of a parser arm that silently never matches.
//!
//! These fail loudly where the discriminator-constant guards skip silently. A
//! skipped constant only withholds an extra, and the IDL compiles as before. The
//! case here leaves an account or instruction that can never parse, with no
//! diagnostic at runtime.

use codama_nodes::{DiscriminatorNode, RootNode};

pub(crate) fn validate_idl(
    idl: &RootNode,
    envelope: Option<&crate::parse::ProgramEnvelope>,
) -> Result<(), String> {
    check_discriminator_widths(idl, envelope)
}

fn check_discriminator_widths(
    idl: &RootNode,
    envelope: Option<&crate::parse::ProgramEnvelope>,
) -> Result<(), String> {
    for ix in &idl.program.instructions {
        let Some((size, len)) = crate::render::instruction_parser::dead_width_discriminator(ix)
        else {
            continue;
        };

        return Err(width_message("instruction", &ix.name, size, len));
    }

    for account in &idl.program.accounts {
        for discriminator in &account.discriminators {
            let DiscriminatorNode::Field(node) = discriminator else {
                continue;
            };

            let Some(field) = crate::render::account_parser::field_discriminator(account, node)
            else {
                continue;
            };

            // An empty default has no bytes to compare, so the arm is a catch-all that
            // matches any data long enough to hold the field.
            if field.bytes.is_empty() || !crate::utils::width_mismatch(field.size, &field.bytes) {
                continue;
            }

            return Err(width_message(
                "account",
                &account.name,
                field.size,
                field.bytes.len(),
            ));
        }
    }

    // Events are only parsed under `program-events`; checking them otherwise would
    // reject IDLs that compile today.
    if !cfg!(feature = "program-events") {
        return Ok(());
    }

    let events = &idl.program.events;

    let event_envelopes = envelope.map_or_else(
        || vec![None; events.len()],
        |program| program.resolve(events),
    );

    for (event, envelope) in events.iter().zip(event_envelopes) {
        let Some((size, len)) =
            crate::render::instruction_parser::dead_width_event_discriminator(event, envelope)
        else {
            continue;
        };

        return Err(width_message("event", &event.name, size, len));
    }

    Ok(())
}

fn width_message(kind: &str, name: &str, size: usize, len: usize) -> String {
    format!(
        "{kind} `{name}` declares a discriminator field of {size} bytes but its default value is \
         {len} bytes, so it can never match; make the declared size and the default agree",
    )
}
