//! Field-level parity with the JS pipeline minus the three unported passes
//! (`fixtures/regenerate.mjs`). The parser-level check against the full CLI
//! output is `shipstern-proc-macro`'s `anchor_parity_tests`.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn fixtures() -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures") }

/// Fixtures with a parity file; pump_fun is covered at parser level only.
fn names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures())
        .expect("fixtures dir is readable")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().to_string();

            name.strip_suffix(".parity.codama.json").map(str::to_owned)
        })
        .collect();

    names.sort();

    names
}

/// JSON paths where the two trees disagree, e.g. `program.instructions[2].arguments[1].type.kind`.
fn diff(path: &str, js: &Value, rust: &Value, out: &mut Vec<String>) {
    match (js, rust) {
        (Value::Object(a), Value::Object(b)) => {
            let keys = a.keys().chain(b.keys().filter(|k| !a.contains_key(*k)));

            for key in keys {
                let child = format!("{path}.{key}");

                diff(
                    &child,
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        },

        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!(
                    "{path}: {} items in JS, {} in Rust",
                    a.len(),
                    b.len()
                ));
            }

            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                diff(&format!("{path}[{i}]"), x, y, out);
            }
        },

        _ if js != rust => out.push(format!("{path}: JS {js}, Rust {rust}")),

        _ => {},
    }
}

#[test]
fn converted_nodes_match_the_js_converter_field_for_field() {
    let names = names();

    assert!(names.len() >= 8, "fixtures went missing: {names:?}");

    let mut failures = Vec::new();

    for name in &names {
        let expected: codama_nodes::RootNode = serde_json::from_slice(
            &std::fs::read(fixtures().join(format!("{name}.parity.codama.json")))
                .expect("read expected"),
        )
        .expect("expected output is a RootNode");

        let idl = std::fs::read(fixtures().join(format!("{name}.anchor.json"))).expect("read IDL");

        let actual = match codama_nodes_from_anchor::root_node_from_anchor(&idl) {
            Ok(root) => root,
            Err(err) => {
                failures.push(format!("{name}: conversion failed: {err}"));
                continue;
            },
        };

        let mut out = Vec::new();

        diff(
            "",
            &serde_json::to_value(&expected).expect("serialize"),
            &serde_json::to_value(&actual).expect("serialize"),
            &mut out,
        );

        failures.extend(out.into_iter().take(5).map(|d| format!("{name}{d}")));
    }

    assert!(
        failures.is_empty(),
        "diverges from the JS converter:\n{}",
        failures.join("\n")
    );
}
