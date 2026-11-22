//! Expansion-time checks that turn a broken IDL into a `compile_error!` naming the
//! offending entries, instead of generated code that fails with duplicate-definition
//! errors that do not name them.

use std::collections::HashMap;

use codama_nodes::{EventNode, RootNode};

use crate::utils::{to_pascal_case, to_snake_case};

pub(crate) fn validate_idl(idl: &RootNode, events: &[EventNode]) -> Result<(), String> {
    let instruction_names = idl.program.instructions.iter().map(|ix| ix.name.as_str());

    check_unique("instruction", instruction_names, |name| {
        let pascal = to_pascal_case(name);

        vec![
            format!("{pascal}Accounts"),
            format!("{pascal}Args"),
            pascal,
            format!("parse_{}", to_snake_case(name)),
        ]
    })?;

    check_unique(
        "account",
        idl.program
            .accounts
            .iter()
            .map(|account| account.name.as_str()),
        |name| vec![to_pascal_case(name)],
    )?;

    // Events are only emitted under `program-events`; checking them otherwise would
    // reject IDLs that compile today.
    if cfg!(feature = "program-events") {
        check_unique(
            "event",
            events.iter().map(|event| event.name.as_str()),
            |name| {
                let pascal = to_pascal_case(name);

                vec![
                    format!("{pascal}Accounts"),
                    format!("{pascal}Args"),
                    pascal,
                    format!("parse_event_{}", to_snake_case(name)),
                ]
            },
        )?;
    }

    Ok(())
}

/// Names that differ only by ASCII case or leading underscores fold to one
/// identifier, and the identifier set is wider than the pascal-case variant: a
/// pascal collision does not imply a snake one (`_abc` and `abc`).
fn check_unique<'a>(
    kind: &str,
    names: impl Iterator<Item = &'a str>,
    idents: impl Fn(&str) -> Vec<String>,
) -> Result<(), String> {
    let mut seen: HashMap<String, &str> = HashMap::new();

    for name in names {
        for ident in idents(name) {
            let Some(first) = seen.insert(ident.clone(), name) else {
                continue;
            };

            return Err(format!(
                "{kind}s `{first}` and `{name}` both generate the identifier `{ident}`; rename \
                 one of them in the IDL",
            ));
        }
    }

    Ok(())
}
