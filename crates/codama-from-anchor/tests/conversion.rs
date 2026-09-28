//! Cases the fixtures in `tests/idls/anchor/` cannot hold, mostly error paths.

use serde_json::{json, Value};
use shipstern_codama_from_anchor::{root_node_from_anchor, Error};

/// Like [`convert_at`], with the item context stripped so tests match the cause.
fn convert(extra: Value) -> Result<Value, Error> {
    fn cause(err: Error) -> Error {
        match err {
            Error::At { error, .. } => cause(*error),
            err => err,
        }
    }

    convert_at(extra).map_err(cause)
}

/// A minimal spec-0.1.0 IDL with `extra` merged over its top-level keys.
fn convert_at(extra: Value) -> Result<Value, Error> {
    let mut idl = json!({
        "address": "Dex1111111111111111111111111111111111111111",
        "metadata": { "name": "t", "version": "0.1.0", "spec": "0.1.0" },
        "instructions": []
    });

    if let (Some(base), Value::Object(extra)) = (idl.as_object_mut(), extra) {
        base.extend(extra);
    }

    let root = root_node_from_anchor(idl)?;

    Ok(serde_json::to_value(root).expect("serialize root"))
}

fn ix(args: Value, accounts: Value) -> Value {
    json!({ "instructions": [{ "name": "go", "discriminator": [9], "accounts": accounts, "args": args }] })
}

#[test]
fn only_spec_0_1_0_is_accepted() {
    let with_metadata =
        |metadata: Value| json!({ "address": "x", "metadata": metadata, "instructions": [] });

    let cases = [
        (
            with_metadata(json!({ "name": "t", "version": "1", "spec": "0.0.0" })),
            Some("0.0.0"),
        ),
        (with_metadata(json!({ "name": "t", "version": "1" })), None),
        (
            json!({ "version": "0.1.0", "name": "legacy", "instructions": [] }),
            None,
        ),
    ];

    for (idl, found) in cases {
        let err = root_node_from_anchor(idl.clone()).expect_err("must fail");

        let Error::UnsupportedSpec { found: actual } = &err else {
            panic!("wrong error for {idl}: {err}");
        };

        assert_eq!(actual.as_deref(), found);
        assert!(
            err.to_string().contains("0.1.0"),
            "error names the supported spec: {err}"
        );
    }
}

/// Nothing is flattened here, so the flattening error would point the wrong way.
#[test]
fn an_argument_named_discriminator_is_rejected() {
    let err = convert(ix(
        json!([{ "name": "discriminator", "type": "u8" }]),
        json!([]),
    ))
    .expect_err("must fail");

    assert!(matches!(err, Error::DiscriminatorArgument), "{err}");
}

/// Flattening `s` gives two arguments named `a`, which would not compile.
#[test]
fn flattening_into_a_duplicate_argument_is_rejected() {
    let mut idl = ix(
        json!([
            { "name": "a", "type": "u8" },
            { "name": "s", "type": { "defined": { "name": "S" } } }
        ]),
        json!([]),
    );

    idl["types"] = json!([
        { "name": "S", "type": { "kind": "struct", "fields": [{ "name": "a", "type": "u64" }] } }
    ]);

    let err = convert(idl).expect_err("must fail");

    assert!(
        matches!(&err, Error::ConflictingFlattenedArguments(names) if names == &["a"]),
        "{err}"
    );
}

/// JS fails the whole IDL on these, as it does on Jupiter Limit Order v2's
/// `unique_id` seed. The parser never reads PDAs, so only the PDA is dropped.
#[test]
fn a_pda_whose_seed_does_not_resolve_is_dropped() {
    let seeds = [
        json!({ "kind": "arg", "path": "ghost" }),
        json!({ "kind": "weird", "path": "a.b" }),
    ];

    for seed in seeds {
        let root = convert(ix(
            json!([]),
            json!([{ "name": "v", "pda": { "seeds": [seed] } }]),
        ))
        .expect("converts");

        let node = &root["program"]["instructions"][0]["accounts"][0];

        assert_eq!(node["name"], "v");
        assert!(node["defaultValue"].is_null(), "{node}");
    }
}

/// JS resolves arguments in the callee's scope, so a reused parameter name
/// takes the callee's binding and Anchor's own `generics.json` overflows it.
#[test]
fn generic_arguments_resolve_in_the_scope_that_wrote_them() {
    let arg = |ty: Value| json!({ "kind": "type", "type": ty });
    let params = json!([{ "kind": "type", "name": "A" }, { "kind": "type", "name": "B" }]);

    let wrapper = json!({ "name": "Wrapper", "generics": params, "type": { "kind": "struct", "fields": [
        { "name": "a", "type": { "generic": "A" } },
        { "name": "b", "type": { "generic": "B" } }
    ] } });

    let outer = json!({ "name": "Outer", "generics": params, "type": { "kind": "struct", "fields": [{
        "name": "w",
        "type": { "defined": { "name": "Wrapper", "generics": [arg(json!({ "generic": "B" })), arg(json!("u8"))] } }
    }] } });

    let user = json!({ "name": "User", "type": { "kind": "struct", "fields": [{
        "name": "o",
        "type": { "defined": { "name": "Outer", "generics": [arg(json!("u16")), arg(json!("u32"))] } }
    }] } });

    let root = convert(json!({ "types": [wrapper, outer, user] })).expect("converts");

    let o = &root["program"]["definedTypes"][0]["type"]["fields"][0]["type"];
    let w = &o["fields"][0]["type"];

    assert_eq!(w["fields"][0]["type"]["format"], "u32", "{w}");
    assert_eq!(w["fields"][1]["type"]["format"], "u8", "{w}");
}

/// Each use expands its argument again, so every level of nesting doubles it.
#[test]
fn exponential_generic_expansion_is_an_error_not_a_hang() {
    let pair = json!({ "name": "Pair", "generics": [{ "kind": "type", "name": "T" }], "type": { "kind": "struct", "fields": [
        { "name": "a", "type": { "generic": "T" } },
        { "name": "b", "type": { "generic": "T" } }
    ] } });

    let mut ty = json!("u8");
    for _ in 0..30 {
        ty = json!({ "defined": { "name": "Pair", "generics": [{ "kind": "type", "type": ty }] } });
    }

    let user = json!({ "name": "User", "type": { "kind": "struct", "fields": [{ "name": "p", "type": ty }] } });

    let err = convert(json!({ "types": [pair, user] })).expect_err("must fail");

    assert!(matches!(err, Error::TooLarge), "{err}");
}

/// A dangling link would only fail later, as a missing type in rustc.
#[test]
fn a_link_to_a_missing_or_generic_type_is_rejected() {
    let link = |name: &str| {
        json!({
            "types": [{ "name": "Wrapper", "generics": [{ "kind": "type", "name": "T" }], "type": { "kind": "struct", "fields": [] } }],
            "instructions": [{ "name": "go", "discriminator": [9], "accounts": [], "args": [
                { "name": "a", "type": { "defined": { "name": name } } }
            ] }]
        })
    };

    assert!(matches!(convert(link("Nope")), Err(Error::UndefinedType(n)) if n == "Nope"));

    let generic_enum = json!({
        "types": [
            { "name": "Maybe", "generics": [{ "kind": "type", "name": "T" }], "type": { "kind": "enum", "variants": [{ "name": "Set", "fields": [{ "generic": "T" }] }] } },
            { "name": "User", "type": { "kind": "struct", "fields": [{ "name": "m", "type": { "defined": { "name": "Maybe", "generics": [{ "kind": "type", "type": "u8" }] } } }] } }
        ]
    });

    assert!(matches!(convert(generic_enum), Err(Error::GenericEnum(n)) if n == "Maybe"));
    assert!(
        matches!(convert(link("Wrapper")), Err(Error::GenericArgsMissing(n)) if n == "Wrapper")
    );
}

#[test]
fn nesting_past_the_limit_is_an_error_not_a_crash() {
    let mut deep = json!("u8");
    for _ in 0..200 {
        deep = json!({ "vec": deep });
    }

    let err =
        convert(ix(json!([{ "name": "a", "type": deep }]), json!([]))).expect_err("must fail");

    assert!(matches!(err, Error::RecursionLimit), "{err}");
    assert!(
        err.to_string().contains("128"),
        "error states the limit: {err}"
    );
}

/// Reading these as an empty variant would skip the variant's bytes on decode.
#[test]
fn an_enum_variant_with_non_array_fields_is_rejected() {
    let types = |fields: Value| json!({ "types": [{ "name": "E", "type": { "kind": "enum", "variants": [{ "name": "A", "fields": fields }] } }] });

    assert!(matches!(
        convert(types(json!({}))),
        Err(Error::UnrecognizedType(_))
    ));
    assert!(convert(types(json!(null))).is_ok());
}

/// Either would generate a nameless field or variant. JS rejects the field but
/// writes the variant with an empty name.
#[test]
fn a_nameless_field_or_variant_is_rejected() {
    let field = json!({ "types": [{ "name": "S", "type": { "kind": "struct", "fields": [{ "name": 5, "type": "u8" }] } }] });
    let variant = json!({ "types": [{ "name": "E", "type": { "kind": "enum", "variants": [{ "fields": [] }] } }] });

    assert!(matches!(convert(field), Err(Error::UnrecognizedType(_))));
    assert!(matches!(convert(variant), Err(Error::UnrecognizedType(_))));
}

/// Read as one account, a malformed group would shift every account after it.
#[test]
fn a_malformed_account_group_is_rejected() {
    let err = convert(ix(
        json!([]),
        json!([{ "name": "grp", "accounts": "junk" }, { "name": "c" }]),
    ))
    .expect_err("must fail");

    assert!(matches!(err, Error::Json(_)), "{err}");
}

/// Empty or digit-leading names panic the renderer. Error names are never
/// rendered, so they pass.
#[test]
fn a_name_that_is_not_an_identifier_is_rejected() {
    for name in ["__", "1st"] {
        let field = json!({ "types": [{ "name": "S", "type": { "kind": "struct", "fields": [{ "name": name, "type": "u8" }] } }] });

        let idls = [
            ix(json!([{ "name": name, "type": "u8" }]), json!([])),
            ix(json!([]), json!([{ "name": name }])),
            field,
        ];

        for idl in idls {
            let err = convert(idl).expect_err("must fail");

            assert!(
                matches!(err, Error::InvalidName(ref n) if n == name),
                "{err}"
            );
        }
    }

    let unrendered = json!({
        "errors": [{ "code": 6000, "name": "1st" }],
        "constants": [{ "name": "1st", "type": "u8", "value": "1" }]
    });

    assert!(convert(unrendered).is_ok());
}

#[test]
fn types_whose_names_camel_case_alike_are_rejected() {
    let unit = |name: &str| json!({ "name": name, "type": { "kind": "struct", "fields": [] } });

    let err = convert_at(json!({ "types": [unit("pool_state"), unit("PoolState")] }))
        .expect_err("must fail");

    assert_eq!(
        err.to_string(),
        "type `PoolState`: same Rust name as `pool_state`"
    );
}

/// An empty discriminator would match every instruction, account or event of
/// the program; the error names the item.
#[test]
fn an_empty_discriminator_is_rejected_with_its_item() {
    let unit = |name: &str| json!({ "name": name, "type": { "kind": "struct", "fields": [] } });

    let cases = [
        (
            json!({ "instructions": [{ "name": "go", "discriminator": [], "accounts": [], "args": [] }] }),
            "instruction `go`",
        ),
        (
            json!({ "accounts": [{ "name": "Pool", "discriminator": [] }], "types": [unit("Pool")] }),
            "account `Pool`",
        ),
        (
            json!({ "events": [{ "name": "Swapped", "discriminator": [] }], "types": [unit("Swapped")] }),
            "event `Swapped`",
        ),
    ];

    for (idl, item) in cases {
        let err = convert_at(idl).expect_err("must fail");

        assert_eq!(err.to_string(), format!("{item}: discriminator is empty"));
    }
}

/// Without this, a bad field anywhere in the IDL reports no location.
#[test]
fn a_malformed_item_is_named() {
    let bad_instruction = json!({ "instructions": [
        { "name": "a", "discriminator": [1], "accounts": [], "args": [] },
        { "name": "b", "discriminator": [1, 256], "accounts": [], "args": [] }
    ] });

    let bad_error = json!({ "errors": [{ "code": "E6000", "name": "E" }] });

    for (idl, item) in [
        (bad_instruction, "instruction `b`"),
        (bad_error, "error `E`"),
    ] {
        let err = convert_at(idl).expect_err("must fail");

        assert!(
            err.to_string()
                .starts_with(&format!("{item}: invalid Anchor IDL JSON")),
            "{err}"
        );
    }
}

/// The parser never renders these, and the Codama loader accepts a string code.
#[test]
fn a_string_error_code_or_bare_constant_value_converts() {
    let root = convert(json!({
        "errors": [{ "code": "6000", "name": "E" }],
        "constants": [{ "name": "MAX", "type": "u8", "value": 5 }]
    }))
    .expect("converts");

    assert_eq!(root["program"]["errors"][0]["code"], 6000);
    assert_eq!(root["program"]["constants"][0]["value"]["number"], 5);
}
