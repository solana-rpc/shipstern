//! Error paths no fixture can hold: an unsupported spec, a PDA seed naming a
//! missing argument, nesting past the limit, a missing program address and
//! enum variant fields that are not an array.
//! Conversion rules are covered end to end by `fixtures/shapes.anchor.json`.

use serde_json::{json, Value};
use shipstern_codama_from_anchor::{root_node_from_anchor, Error};

/// A minimal spec-0.1.0 IDL with `extra` merged over its top-level keys.
fn convert(extra: Value) -> Result<Value, Error> {
    let mut idl = json!({
        "address": "Dex1111111111111111111111111111111111111111",
        "metadata": { "name": "t", "version": "0.1.0", "spec": "0.1.0" },
        "instructions": []
    });

    if let (Some(base), Value::Object(extra)) = (idl.as_object_mut(), extra) {
        base.extend(extra);
    }

    let root = root_node_from_anchor(&serde_json::to_vec(&idl).expect("serialize IDL"))?;

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
        let err = root_node_from_anchor(&serde_json::to_vec(&idl).expect("serialize"))
            .expect_err("must fail");

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

#[test]
fn a_seed_naming_a_missing_argument_fails() {
    let err = convert(ix(
        json!([]),
        json!([{ "name": "v", "pda": { "seeds": [{ "kind": "arg", "path": "ghost" }] } }]),
    ))
    .expect_err("must fail");

    assert!(
        matches!(err, Error::ArgumentTypeMissing(ref name) if name == "ghost"),
        "{err}"
    );
}

/// A self-referential generic, as in Anchor's own `tests/idl/idls/generics.json`,
/// overflows the JS converter's stack; here it hits the nesting limit instead.
#[test]
fn nesting_past_the_limit_is_an_error_not_a_crash() {
    let wrapper = json!({
        "name": "Wrapper",
        "generics": [{ "kind": "type", "name": "T" }],
        "type": { "kind": "struct", "fields": [{ "name": "inner", "type": { "generic": "T" } }] }
    });

    let outer = json!({
        "name": "Outer",
        "generics": [{ "kind": "type", "name": "T" }],
        "type": { "kind": "struct", "fields": [{
            "name": "w",
            "type": { "defined": { "name": "Wrapper", "generics": [{ "kind": "type", "type": { "generic": "T" } }] } }
        }] }
    });

    let user = json!({ "name": "User", "type": { "kind": "struct", "fields": [{
        "name": "o",
        "type": { "defined": { "name": "Outer", "generics": [{ "kind": "type", "type": "u8" }] } }
    }] } });

    let err = convert(json!({ "types": [wrapper, outer, user] })).expect_err("must fail");

    assert!(matches!(err, Error::RecursionLimit), "{err}");

    // Plain nesting past the limit gets the same error, not serde_json's.
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

/// JS converts this, but its output lacks `publicKey`, which shipstern needs for
/// `PROGRAM_ID` and fails to load; rejecting here reports it one step earlier.
#[test]
fn an_idl_without_an_address_is_rejected() {
    let idl = json!({
        "metadata": { "name": "t", "version": "0.1.0", "spec": "0.1.0" },
        "instructions": []
    });

    let err = root_node_from_anchor(&serde_json::to_vec(&idl).expect("serialize"))
        .expect_err("must fail");

    assert!(err.to_string().contains("address"), "{err}");
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
