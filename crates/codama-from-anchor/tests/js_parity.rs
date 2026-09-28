//! The full Codama JSON must match the JS output once extractPdas,
//! setInstructionAccountDefaultValues and setFixedAccountSizes are undone. This
//! also covers errors, constants, PDA seeds and docs, which the parser never reads.

use std::path::{Path, PathBuf};

use serde_json::Value;
use shipstern_codama_from_anchor::root_node_from_anchor;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/idls/anchor")
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));

    serde_json::from_str(&text).unwrap_or_else(|err| panic!("parse {}: {err}", path.display()))
}

fn undo_unported_passes(js: &mut Value) {
    // Taking leaves `null`, which matches the key Rust omits for no PDAs.
    let pdas = js
        .pointer_mut("/program/pdas")
        .map(Value::take)
        .unwrap_or_default();

    inline_pda_links(js, pdas.as_array().map_or(&[], Vec::as_slice));

    if let Some(accounts) = js
        .pointer_mut("/program/accounts")
        .and_then(Value::as_array_mut)
    {
        for account in accounts {
            account["size"] = Value::Null;
        }
    }

    // Only the defaults the pass invents; a `publicKeyValueNode` can also come
    // from the IDL's `address`, so it stays and a dropped address still fails.
    if let Some(instructions) = js
        .pointer_mut("/program/instructions")
        .and_then(Value::as_array_mut)
    {
        for account in instructions
            .iter_mut()
            .filter_map(|ix| ix.get_mut("accounts").and_then(Value::as_array_mut))
            .flatten()
        {
            let kind = account
                .pointer("/defaultValue/kind")
                .and_then(Value::as_str);

            if matches!(
                kind,
                Some("payerValueNode" | "identityValueNode" | "programIdValueNode")
            ) && let Some(account) = account.as_object_mut()
            {
                account.remove("defaultValue");
            }
        }
    }
}

/// Replace each `pdaLinkNode` with the hoisted `pdaNode` it names.
fn inline_pda_links(value: &mut Value, pdas: &[Value]) {
    if value.get("kind").and_then(Value::as_str) == Some("pdaLinkNode") {
        let name = value.get("name").and_then(Value::as_str);

        if let Some(pda) = pdas
            .iter()
            .find(|p| p.get("name").and_then(Value::as_str) == name)
        {
            *value = pda.clone();
        }

        return;
    }

    match value {
        Value::Object(map) => map.values_mut().for_each(|v| inline_pda_links(v, pdas)),
        Value::Array(items) => items.iter_mut().for_each(|v| inline_pda_links(v, pdas)),
        _ => {},
    }
}

/// extractPdas renames PDAs on collision, a listed divergence, so names are dropped.
fn strip_pda_names(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("kind").and_then(Value::as_str) == Some("pdaNode") {
                map.remove("name");
            }

            map.values_mut().for_each(strip_pda_names);
        },
        Value::Array(items) => items.iter_mut().for_each(strip_pda_names),
        _ => {},
    }
}

fn first_difference(path: &str, rust: &Value, js: &Value) -> Option<String> {
    match (rust, js) {
        (Value::Object(r), Value::Object(j)) => {
            let keys = r.keys().chain(j.keys().filter(|k| !r.contains_key(*k)));

            keys.filter_map(|k| {
                let (r, j) = (
                    r.get(k).unwrap_or(&Value::Null),
                    j.get(k).unwrap_or(&Value::Null),
                );

                first_difference(&format!("{path}.{k}"), r, j)
            })
            .next()
        },
        (Value::Array(r), Value::Array(j)) if r.len() == j.len() => r
            .iter()
            .zip(j)
            .enumerate()
            .find_map(|(i, (r, j))| first_difference(&format!("{path}[{i}]"), r, j)),
        _ if rust != js => Some(format!("{path}:\n  rust: {rust}\n  js:   {js}")),
        _ => None,
    }
}

#[test]
fn converter_json_matches_js_output() {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures dir is readable")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;

            name.strip_suffix(".anchor.json").map(str::to_owned)
        })
        .collect();

    names.sort();

    assert!(
        !names.is_empty(),
        "no fixtures in {}",
        fixtures_dir().display()
    );

    for name in names {
        let anchor = read_json(&fixtures_dir().join(format!("{name}.anchor.json")));
        let mut js = read_json(&fixtures_dir().join(format!("{name}.codama.json")));

        let root = root_node_from_anchor(anchor).unwrap_or_else(|err| panic!("{name}: {err}"));
        let mut rust = serde_json::to_value(root).expect("serialize root");

        undo_unported_passes(&mut js);
        strip_pda_names(&mut js);
        strip_pda_names(&mut rust);

        if let Some(diff) = first_difference("", &rust, &js) {
            panic!("{name} differs from the JS output at {diff}");
        }
    }
}
