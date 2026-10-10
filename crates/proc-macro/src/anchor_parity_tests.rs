//! A parser generated from an Anchor IDL through the macro's loader must be
//! byte-identical to one generated from the pinned JS output for that IDL.

use std::path::{Path, PathBuf};

use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/idls/anchor")
}

fn tokens(idl: &Path) -> proc_macro2::TokenStream {
    crate::expand_parser_tokens(
        idl,
        crate::render::shipstern_parser::ParserConfig::default(),
        false,
    )
}

/// Formatted so two expansions that disagree give a line-oriented diff.
fn expand_pretty(idl: &Path) -> String {
    let file: syn::File = syn::parse2(tokens(idl))
        .unwrap_or_else(|err| panic!("{} expanded to invalid Rust: {err}", idl.display()));

    prettyplease::unparse(&file)
}

fn expand_verifiable_anchor(name: &str) -> String {
    let source = fixtures_dir().join(format!("{name}.anchor.json"));

    if name != "anchor_external" {
        return expand_pretty(&source);
    }

    let mut idl: Value = serde_json::from_slice(
        &std::fs::read(&source).unwrap_or_else(|err| panic!("read {}: {err}", source.display())),
    )
    .unwrap_or_else(|err| panic!("parse {}: {err}", source.display()));

    let packed = idl["types"]
        .as_array_mut()
        .and_then(|types| types.iter_mut().find(|ty| ty["name"] == "PackedAccount"))
        .expect("PackedAccount fixture");

    packed["repr"] = serde_json::json!({ "kind": "c" });

    let path = std::env::temp_dir().join(format!(
        "shipstern-anchor-parity-{}.json",
        std::process::id()
    ));

    std::fs::write(&path, serde_json::to_vec(&idl).expect("serialize IDL"))
        .expect("write temporary IDL");

    let expanded = expand_pretty(&path);
    let _ = std::fs::remove_file(path);

    expanded
}

fn first_divergence(expected: &str, actual: &str) -> String {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();

    let at = expected
        .iter()
        .zip(&actual)
        .position(|(e, a)| e != a)
        .unwrap_or(expected.len().min(actual.len()));

    format!(
        "line {}:\n  js:   {}\n  rust: {}\n  ({} vs {} lines)",
        at + 1,
        expected.get(at).unwrap_or(&"<eof>"),
        actual.get(at).unwrap_or(&"<eof>"),
        expected.len(),
        actual.len(),
    )
}

fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures dir is readable")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().to_string();

            name.strip_suffix(".anchor.json").map(str::to_owned)
        })
        .collect();

    names.sort();

    names
}

#[test]
fn anchor_idls_generate_the_same_parser_as_the_js_output() {
    let names = fixture_names();

    assert!(
        !names.is_empty(),
        "no fixtures found in {}",
        fixtures_dir().display()
    );

    let mut failures = Vec::new();

    for name in &names {
        let expected = expand_pretty(&fixtures_dir().join(format!("{name}.codama.json")));

        // An expected parser that is itself a compile error would make the
        // comparison vacuous.
        assert!(
            !expected.contains("compile_error"),
            "{name}: the JS-produced fixture does not expand cleanly",
        );

        let actual = expand_verifiable_anchor(name);

        if actual.contains("compile_error") {
            failures.push(format!("{name}: {actual}"));
        } else if expected != actual {
            failures.push(format!("{name}: {}", first_divergence(&expected, &actual)));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} fixtures diverge from the JS converter:\n{}",
        failures.len(),
        names.len(),
        failures.join("\n"),
    );
}

#[test]
fn an_unverifiable_anchor_layout_fails_to_compile() {
    let expanded = tokens(&fixtures_dir().join("anchor_external.anchor.json")).to_string();

    assert!(
        expanded.contains("compile_error"),
        "no compile_error: {expanded}"
    );
    assert!(
        expanded.contains("layout cannot be computed"),
        "layout error missing: {expanded}"
    );
}

#[test]
fn an_unsupported_anchor_idl_fails_to_compile_with_the_convert_hint() {
    let path = std::env::temp_dir().join(format!(
        "shipstern-unknown-spec-{}.json",
        std::process::id()
    ));

    std::fs::write(
        &path,
        r#"{"address":"x","metadata":{"name":"t","version":"1","spec":"0.0.0"},"instructions":[]}"#,
    )
    .expect("write IDL");

    let expanded = tokens(&path).to_string();

    let _ = std::fs::remove_file(&path);

    assert!(
        expanded.contains("compile_error"),
        "no compile_error: {expanded}"
    );
    assert!(
        expanded.contains("codama convert"),
        "convert hint missing: {expanded}"
    );
}
