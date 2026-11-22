//! Expansion-time checks that turn a broken IDL into a `compile_error!` naming the
//! offending entry, instead of a parser arm that silently never matches.
//!
//! These fail loudly where the discriminator-constant guards skip silently. A
//! skipped constant only withholds an extra, and the IDL compiles as before. The
//! case here leaves an account or instruction that can never parse, with no
//! diagnostic at runtime.

use codama_nodes::{DiscriminatorNode, RootNode};

pub(crate) fn validate_idl(idl: &RootNode) -> Result<(), String> { check_discriminator_widths(idl) }

fn check_discriminator_widths(idl: &RootNode) -> Result<(), String> {
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

            // An empty default matches any data of the declared width, so its arm is live.
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

    Ok(())
}

fn width_message(kind: &str, name: &str, size: usize, len: usize) -> String {
    format!(
        "{kind} `{name}` declares a {size}-byte discriminator field but its default value is \
         {len} bytes, so it can never match; make the declared size and the default agree",
    )
}
