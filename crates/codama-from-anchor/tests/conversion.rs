//! Cases the fixtures in `tests/idls/anchor/` cannot hold, mostly error paths.

use serde_json::{json, Value};
use shipstern_codama_from_anchor::{root_node_from_anchor, Error, ItemKind};

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

fn unit(name: &str) -> Value { json!({ "name": name, "type": { "kind": "struct", "fields": [] } }) }

#[test]
fn an_unknown_spec_is_rejected() {
    let metadata = json!({ "name": "t", "version": "1", "spec": "0.0.0" });
    let err = root_node_from_anchor(json!({ "address": "x", "metadata": metadata }))
        .expect_err("must fail");

    assert!(
        matches!(&err, Error::UnsupportedSpec { found: Some(s) } if s == "0.0.0"),
        "{err}"
    );
    assert!(
        err.to_string().contains("0.1.0"),
        "error names the supported spec: {err}"
    );

    // Not the legacy shape either, so it is not sent to the upgrade.
    let err = root_node_from_anchor(json!({})).expect_err("must fail");

    assert!(
        matches!(err, Error::UnsupportedSpec { found: None }),
        "{err}"
    );
}

/// A legacy IDL with `extra` merged over its top-level keys.
fn legacy(extra: Value) -> Value {
    let mut idl = json!({
        "version": "0.1.0",
        "name": "legacy",
        "metadata": { "address": "11111111111111111111111111111111" },
        "instructions": [{
            "name": "doThing",
            "accounts": [{ "name": "vault", "isMut": true, "isSigner": false }],
            "args": []
        }]
    });

    if let (Some(base), Value::Object(extra)) = (idl.as_object_mut(), extra) {
        base.extend(extra);
    }

    idl
}

#[test]
fn legacy_idls_the_upgrade_would_misread_are_refused() {
    let mut shank = legacy(json!({}));
    shank["metadata"]["origin"] = json!("shank");

    let mut discriminant = legacy(json!({}));
    discriminant["instructions"][0]["discriminant"] = json!({ "type": "u8", "value": 0 });

    let mut missing_flag = legacy(json!({}));
    missing_flag["instructions"][0]["accounts"][0]
        .as_object_mut()
        .expect("account object")
        .remove("isMut");

    let generic = json!({ "types": [{
        "name": "G",
        "generics": ["T"],
        "type": { "kind": "struct", "fields": [{ "name": "v", "type": { "generic": "T" } }] }
    }]});

    let struct_of = |field: &str, ty: &str| json!({ "kind": "struct", "fields": [{ "name": field, "type": ty }] });
    let shadowed = json!({
        "accounts": [{ "name": "Config", "type": struct_of("fee", "u64") }],
        "types": [{ "name": "Config", "type": struct_of("flag", "u8") }]
    });

    let own_discriminator = json!({ "accounts": [{
        "name": "Stake",
        "discriminator": [1, 2, 3, 4, 5, 6, 7, 8],
        "type": struct_of("a", "u64")
    }]});

    let mut no_version = legacy(json!({}));
    no_version
        .as_object_mut()
        .expect("idl object")
        .remove("version");

    type Check = fn(&Error) -> bool;

    let cases: [(&str, Value, Check); 10] = [
        ("shank origin", shank, |err| {
            matches!(err, Error::NotAnchor(_))
        }),
        ("own discriminant", discriminant, |err| {
            matches!(err, Error::NotAnchor(_))
        }),
        ("no address", legacy(json!({ "metadata": {} })), |err| {
            matches!(err, Error::MissingAddress)
        }),
        (
            "state",
            legacy(json!({ "state": { "struct": {}, "methods": [] } })),
            |err| matches!(err, Error::LegacyUnsupported(_)),
        ),
        ("own discriminator", legacy(own_discriminator), |err| {
            matches!(err, Error::NotAnchor(_))
        }),
        (
            "generic account",
            legacy(json!({ "accounts": generic["types"] })),
            |err| {
                matches!(err, Error::At { kind: ItemKind::Account, name, error }
                if name == "G" && matches!(**error, Error::LegacyUnsupported(_)))
            },
        ),
        ("generic type", legacy(generic), |err| {
            matches!(err, Error::At { name, error, .. }
                if name == "G" && matches!(**error, Error::LegacyUnsupported(_)))
        }),
        ("no version", no_version, |err| {
            matches!(err, Error::Legacy(_))
        }),
        ("malformed account", missing_flag, |err| {
            matches!(err, Error::At { name, error, .. }
                if name == "doThing" && matches!(**error, Error::Legacy(_)))
        }),
        ("type shadowing an account", legacy(shadowed), |err| {
            matches!(err, Error::At { name, error, .. }
                if name == "Config" && matches!(**error, Error::LegacyUnsupported(_)))
        }),
    ];

    for (case, idl, expected) in cases {
        let err = root_node_from_anchor(idl).expect_err(case);

        assert!(expected(&err), "{case}: {err}");
    }
}

/// Pyth's receiver IDL names its `PriceUpdateV2` account `priceUpdateV2`, and an
/// underscore is part of the Rust name; both must hash as on chain.
#[test]
fn legacy_accounts_hash_their_rust_name() {
    let idl = legacy(json!({
        "accounts": [
            { "name": "priceUpdateV2", "type": { "kind": "struct", "fields": [] } },
            { "name": "Pool_State", "type": { "kind": "struct", "fields": [] } }
        ]
    }));

    let root = serde_json::to_value(root_node_from_anchor(idl).expect("converts"))
        .expect("serialize root");

    let discriminators: Vec<_> = (0..2)
        .map(|i| &root["program"]["accounts"][i]["data"]["fields"][0]["defaultValue"]["data"])
        .collect();

    assert_eq!(discriminators, ["22f123639d7ef4cd", "aabd5e99fc91f0ad"]);
}

/// `pool_state` cannot be a Rust type name, so it hashes as `PoolState`.
#[test]
fn a_snake_case_legacy_account_hashes_its_pascal_case_name() {
    let idl = legacy(json!({
        "accounts": [{ "name": "pool_state", "type": { "kind": "struct", "fields": [] } }]
    }));

    let root = serde_json::to_value(root_node_from_anchor(idl).expect("converts"))
        .expect("serialize root");

    assert_eq!(
        root["program"]["accounts"][0]["data"]["fields"][0]["defaultValue"]["data"],
        json!("f7ede3f5d7c3de46")
    );
}

/// Anchor 0.29 writes Rust `set_a_b` as `setAB`, which heck 0.3 reads back as `set_ab`.
#[test]
fn a_legacy_instruction_with_single_letter_words_hashes_its_rust_name() {
    let mut idl = legacy(json!({}));
    idl["instructions"][0]["name"] = json!("setAB");

    let root = serde_json::to_value(root_node_from_anchor(idl).expect("converts"))
        .expect("serialize root");

    let ix = &root["program"]["instructions"][0];

    assert_eq!(
        (&ix["name"], &ix["arguments"][0]["defaultValue"]["data"]),
        (&json!("setAB"), &json!("00f39ea546b3f5e9"))
    );
}

/// Neither a const seed the upgrade cannot read (the parser never reads PDAs) nor
/// Anchor 0.29's `{"defined": "usize"}` constant type may stop the conversion.
#[test]
fn legacy_fields_the_upgrade_cannot_read_still_convert() {
    let mut idl = legacy(json!({
        "constants": [{ "name": "MAX", "type": { "defined": "usize" }, "value": "8" }]
    }));
    idl["instructions"][0]["accounts"][0]["pda"] = json!({
        "seeds": [{ "kind": "const", "type": { "array": ["u8", 5] }, "value": [1, 2, 3, 4, 5] }]
    });

    root_node_from_anchor(idl).expect("converts");
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

/// JS resolves arguments in the callee's scope, so a reused parameter name takes
/// the callee's binding, and on Anchor's own `generics.json` JS overflows the stack.
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

/// Anchor passes `N` on to `Inner` as a type argument; JS writes no count there.
#[test]
fn a_const_generic_passed_on_as_a_type_argument_keeps_its_value() {
    let generic = |name: &str, field: Value| {
        json!({ "name": name, "generics": [{ "kind": "const", "name": "N", "type": "usize" }],
                "type": { "kind": "struct", "fields": [{ "name": "f", "type": field }] } })
    };

    let inner = generic("Inner", json!({ "array": ["u16", { "generic": "N" }] }));
    let outer = generic(
        "Outer",
        json!({ "defined": { "name": "Inner", "generics": [{ "kind": "type", "type": { "generic": "N" } }] } }),
    );
    let holder = json!({ "name": "Holder", "type": { "kind": "struct", "fields": [
        { "name": "o", "type": { "defined": { "name": "Outer", "generics": [{ "kind": "const", "value": "3" }] } } }
    ] } });

    let root = convert(json!({ "types": [inner, outer, holder] })).expect("converts");
    let array = &root["program"]["definedTypes"][0]["type"]["fields"][0]["type"]["fields"][0]
        ["type"]["fields"][0]["type"];

    assert_eq!(array["count"]["value"], 3, "{array}");
}

/// Each use expands its argument again, so every level of nesting doubles it. The
/// second shape doubles only the bound argument, which no level renders.
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

    let t = json!({ "kind": "type", "type": { "generic": "T" } });
    let params = json!([{ "kind": "type", "name": "T" }]);

    let mut types = vec![
        json!({ "name": "Two", "generics": [{ "kind": "type", "name": "A" }, { "kind": "type", "name": "B" }],
                "type": { "kind": "struct", "fields": [] } }),
        json!({ "name": "User", "type": { "kind": "struct", "fields": [
            { "name": "g", "type": { "defined": { "name": "G0", "generics": [{ "kind": "type", "type": "u8" }] } } }
        ] } }),
        json!({ "name": "G16", "generics": params, "type": { "kind": "struct", "fields": [] } }),
    ];

    for i in 0..16 {
        let two =
            json!({ "kind": "type", "type": { "defined": { "name": "Two", "generics": [t, t] } } });

        types.push(json!({ "name": format!("G{i}"), "generics": params, "type": { "kind": "struct", "fields": [
            { "name": "f", "type": { "defined": { "name": format!("G{}", i + 1), "generics": [two] } } }
        ] } }));
    }

    let err = convert(json!({ "types": types })).expect_err("must fail");

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

    let unbound = json!({ "types": [
        { "name": "Box", "generics": [{ "kind": "type", "name": "T" }], "type": { "kind": "struct", "fields": [{ "name": "v", "type": { "generic": "T" } }] } },
        { "name": "User", "type": { "kind": "struct", "fields": [
            { "name": "b", "type": { "defined": { "name": "Box", "generics": [{ "kind": "type", "type": { "generic": "T" } }] } } }
        ] } }
    ] });

    assert!(matches!(convert(unbound), Err(Error::GenericArgMissing(n)) if n == "T"));
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

    // Each level wraps the bound argument deeper, though no level renders it.
    let mut wrapped = json!({ "generic": "T" });
    for _ in 0..50 {
        wrapped = json!({ "vec": wrapped });
    }

    let params = json!([{ "kind": "type", "name": "T" }]);
    let mut types = vec![
        json!({ "name": "G3", "generics": params, "type": { "kind": "struct", "fields": [] } }),
    ];

    for i in 0..3 {
        types.push(json!({ "name": format!("G{i}"), "generics": params, "type": { "kind": "struct", "fields": [
            { "name": "f", "type": { "defined": { "name": format!("G{}", i + 1), "generics": [{ "kind": "type", "type": wrapped }] } } }
        ] } }));
    }

    types.push(json!({ "name": "User", "type": { "kind": "struct", "fields": [
        { "name": "g", "type": { "defined": { "name": "G0", "generics": [{ "kind": "type", "type": "u8" }] } } }
    ] } }));

    let err = convert(json!({ "types": types })).expect_err("must fail");

    assert!(matches!(err, Error::RecursionLimit), "{err}");

    // The renderer expands aliases recursively: a long chain overflows its stack
    // and a cycle panics it.
    let alias = |name: String, target: String| json!({ "name": name, "type": { "kind": "type", "alias": { "defined": { "name": target } } } });

    let chain: Vec<Value> = (0..200)
        .map(|i| alias(format!("A{i}"), format!("A{}", i + 1)))
        .chain([json!({ "name": "A200", "type": { "kind": "type", "alias": "u8" } })])
        .collect();

    for types in [json!(chain), json!([alias("A".into(), "A".into())])] {
        let err = convert(json!({ "types": types })).expect_err("must fail");

        assert!(matches!(err, Error::RecursionLimit), "{err}");
    }
}

/// `codama convert` writes these too, and the renderer panics on them or the
/// generated parser allocates a length it never reads.
#[test]
fn shapes_the_generated_parser_cannot_handle_are_rejected() {
    let arg = |ty: Value| ix(json!([{ "name": "a", "type": ty }]), json!([]));
    let inline_enum = json!({ "kind": "enum", "variants": [{ "name": "X" }, { "name": "Y" }] });

    assert!(matches!(
        convert(arg(inline_enum.clone())),
        Err(Error::InlineEnum)
    ));

    let mut alias_to_enum =
        arg(json!({ "defined": { "name": "G", "generics": [{ "kind": "type", "type": "u8" }] } }));

    alias_to_enum["types"] = json!([
        { "name": "G", "generics": [{ "kind": "type", "name": "T" }], "type": { "kind": "type", "alias": inline_enum } }
    ]);
    assert!(matches!(convert(alias_to_enum), Err(Error::InlineEnum)));

    assert!(matches!(
        convert(arg(json!({ "coption": "string" }))),
        Err(Error::VariableSizeCOption)
    ));
    assert!(convert(arg(json!({ "coption": { "array": ["u64", 2] } }))).is_ok());

    assert!(matches!(
        convert(arg(json!({ "array": ["u8", 16_777_216] }))),
        Err(Error::InvalidArrayLength(_))
    ));
    assert!(matches!(
        convert(arg(json!({ "array": ["u64", 2_000_000] }))),
        Err(Error::FixedTypeTooLarge)
    ));
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

/// Anchor writes the first form, as in Raydium CLMM's `TickArrayBitmap`. JS
/// 1.5.6 reads only the second.
#[test]
fn an_anchor_type_alias_converts_like_a_codama_one() {
    let target = json!({ "array": ["u64", 8] });

    let alias = |ty: Value| {
        let root =
            convert(json!({ "types": [{ "name": "Bitmap", "type": ty }] })).expect("converts");

        root["program"]["definedTypes"][0]["type"].clone()
    };

    let anchor = alias(json!({ "kind": "type", "alias": target }));

    assert_eq!(anchor["kind"], "arrayTypeNode");
    assert_eq!(anchor, alias(json!({ "kind": "alias", "value": target })));
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

/// Each pair would render the same Rust items twice.
#[test]
fn items_whose_names_camel_case_alike_are_rejected() {
    let go = |name: &str, d: u8| json!({ "name": name, "discriminator": [d], "accounts": [], "args": [] });
    let tagged = |name: &str, d: u8| json!({ "name": name, "discriminator": [d] });

    let cases = [
        (
            json!({ "types": [unit("pool_state"), unit("PoolState")] }),
            "type `PoolState`: same Rust name as `pool_state`",
        ),
        (
            json!({ "instructions": [go("swap", 1), go("swap_", 2)] }),
            "instruction `swap_`: same Rust name as `swap`",
        ),
        (
            json!({ "accounts": [tagged("Pool", 1), tagged("pool", 2)], "types": [unit("Pool"), unit("pool")] }),
            "account `pool`: same Rust name as `Pool`",
        ),
        (
            json!({ "events": [tagged("Swapped", 1), tagged("swapped", 2)], "types": [unit("Swapped"), unit("swapped")] }),
            "event `swapped`: same Rust name as `Swapped`",
        ),
    ];

    for (idl, message) in cases {
        let err = convert_at(idl).expect_err("must fail");

        assert_eq!(err.to_string(), message);
    }
}

/// Zero-copy memory exists only in accounts, so `types[0]` is declared as one.
fn zero_copy_idl(types: Value) -> Value {
    let name = types[0]["name"].clone();

    json!({ "types": types, "accounts": [{ "name": name, "discriminator": [1] }] })
}

const ACCOUNT: &str = "/program/accounts/0/data/fields";
const NESTED: &str = "/program/definedTypes/0/type/fields";

/// The fields at `pointer` as `[name, size]`, without the account discriminator;
/// only padding has a size.
fn field_sizes(types: Value, pointer: &str) -> Value {
    let root = convert(zero_copy_idl(types)).expect("converts");

    root.pointer(pointer)
        .and_then(Value::as_array)
        .expect("fields")
        .iter()
        .filter(|f| f["name"] != "discriminator")
        .map(|f| json!([f["name"], f["type"]["size"]]))
        .collect()
}

fn zero_copy_c(name: &str, fields: Value) -> Value {
    json!({
        "name": name,
        "serialization": "bytemuckunsafe",
        "repr": { "kind": "c" },
        "type": { "kind": "struct", "fields": fields }
    })
}

/// Borsh would read `b` at offset 1; `repr(C)` puts it at 8.
#[test]
fn a_zero_copy_c_struct_gets_its_implicit_padding() {
    let fields = |repr: Value| {
        let mut ty = zero_copy_c(
            "R",
            json!([{ "name": "a", "type": "u8" }, { "name": "b", "type": "u64" }]),
        );

        ty["repr"] = repr;
        field_sizes(json!([ty]), ACCOUNT)
    };

    assert_eq!(
        fields(json!({ "kind": "c" })),
        json!([["a", null], ["paddingBeforeB", 7], ["b", null]])
    );
    assert_eq!(
        fields(json!({ "kind": "c", "packed": true })),
        json!([["a", null], ["b", null]])
    );
    // A real field already named like the padding keeps its name.
    let taken = zero_copy_c(
        "T",
        json!([{ "name": "paddingBeforeB", "type": "u8" }, { "name": "b", "type": "u64" }]),
    );

    assert_eq!(
        field_sizes(json!([taken]), ACCOUNT),
        json!([["paddingBeforeB", null], ["paddingBeforeB2", 7], [
            "b", null
        ]])
    );
}

/// An alias field takes its target's layout. Past the nesting limit a chain is an
/// error, not a stack overflow.
#[test]
fn an_alias_field_in_a_zero_copy_struct_is_laid_out_as_its_target() {
    let alias = |name: &str, body: Value| json!({ "name": name, "type": body });
    let link = |name: &str| json!({ "defined": { "name": name } });

    let ty = zero_copy_c(
        "R",
        json!([{ "name": "a", "type": "u8" }, { "name": "b", "type": link("Amount") }]),
    );

    let amount = alias("Amount", json!({ "kind": "type", "alias": "u64" }));

    assert_eq!(
        field_sizes(json!([ty, amount]), ACCOUNT),
        json!([["a", null], ["paddingBeforeB", 7], ["b", null]])
    );

    let chain: Vec<Value> = (0..200)
        .map(|i| {
            alias(
                &format!("A{i}"),
                json!({ "kind": "type", "alias": link(&format!("A{}", i + 1)) }),
            )
        })
        .chain([alias("A200", json!({ "kind": "type", "alias": "u64" }))])
        .collect();

    let mut types = vec![zero_copy_c(
        "R",
        json!([{ "name": "a", "type": "u8" }, { "name": "b", "type": link("A0") }]),
    )];

    types.extend(chain);

    let err = convert(zero_copy_idl(json!(types))).expect_err("must fail");

    assert!(matches!(err, Error::UnknownLayout), "{err}");
}

/// Anchor labels a nested struct Borsh unless it derives `zero_copy(unsafe)`
/// itself, yet it still sits in zero-copy memory with its own padding.
#[test]
fn a_struct_nested_in_a_zero_copy_struct_gets_its_padding() {
    let inner = json!({ "name": "Inner", "repr": { "kind": "c" }, "type": { "kind": "struct", "fields": [
        { "name": "x", "type": "u8" },
        { "name": "y", "type": "u64" }
    ] } });

    let outer = zero_copy_c(
        "Outer",
        json!([{ "name": "inner", "type": { "defined": { "name": "Inner" } } }]),
    );

    assert_eq!(
        field_sizes(json!([outer.clone(), inner.clone()]), NESTED),
        json!([["x", null], ["paddingBeforeY", 7], ["y", null]])
    );

    // A `bytemuck` derive checks only its own fields, not those of an `unsafe` struct in it.
    let mut safe_holder = outer;
    safe_holder["serialization"] = json!("bytemuck");

    let mut unsafe_inner = inner;
    unsafe_inner["serialization"] = json!("bytemuckunsafe");

    assert_eq!(
        field_sizes(json!([safe_holder, unsafe_inner]), NESTED),
        json!([["x", null], ["paddingBeforeY", 7], ["y", null]])
    );
}

/// Skipping the padding would misread every field after a gap.
#[test]
fn a_zero_copy_struct_with_an_unknown_layout_is_rejected() {
    let types = |field: Value| {
        json!([
            { "name": "Data", "type": { "kind": "enum", "variants": [{ "name": "A", "fields": ["u64"] }, { "name": "B" }] } },
            { "name": "One", "type": { "kind": "enum", "variants": [{ "name": "A" }] } },
            { "name": "Repr", "repr": { "kind": "rust" }, "type": { "kind": "enum", "variants": [{ "name": "A" }, { "name": "B" }] } },
            { "name": "Reordered", "repr": { "kind": "rust" }, "type": { "kind": "struct", "fields": [
                { "name": "a", "type": "u8" },
                { "name": "b", "type": "u64" }
            ] } },
            { "name": "NoRepr", "type": { "kind": "struct", "fields": [
                { "name": "a", "type": "u8" },
                { "name": "b", "type": "u64" }
            ] } },
            { "name": "PackedBool", "repr": { "kind": "rust", "packed": true }, "type": { "kind": "struct", "fields": [
                { "name": "a", "type": "u64" },
                { "name": "b", "type": "bool" }
            ] } },
            { "name": "PackedRust", "repr": { "kind": "rust", "packed": true }, "type": { "kind": "struct", "fields": [
                { "name": "a", "type": "u8" },
                { "name": "b", "type": "u64" }
            ] } },
            { "name": "AlignedZst", "repr": { "kind": "c", "align": 8 }, "type": { "kind": "struct", "fields": [] } },
            { "name": "BadTransparent", "repr": { "kind": "transparent" }, "type": { "kind": "struct", "fields": [
                { "name": "value", "type": "u64" },
                { "name": "marker", "type": { "defined": { "name": "AlignedZst" } } }
            ] } },
            { "name": "Wrap", "generics": [{ "kind": "type", "name": "T" }], "type": { "kind": "struct", "fields": [
                { "name": "v", "type": { "generic": "T" } }
            ] } },
            zero_copy_c("Outer", json!([{ "name": "x", "type": field }]))
        ])
    };

    let idl = |field: Value| {
        let mut idl = json!({ "types": types(field) });

        idl["accounts"] = json!([{ "name": "Outer", "discriminator": [1] }]);
        idl
    };

    let unknown = [
        json!({ "option": "u64" }),
        json!({ "defined": { "name": "Data" } }),
        json!({ "defined": { "name": "One" } }),
        json!({ "defined": { "name": "Repr" } }),
        json!({ "defined": { "name": "Reordered" } }),
        json!({ "defined": { "name": "NoRepr" } }),
        json!({ "defined": { "name": "PackedBool" } }),
        json!({ "defined": { "name": "PackedRust" } }),
        json!({ "defined": { "name": "BadTransparent" } }),
        json!({ "defined": { "name": "Wrap", "generics": [{ "kind": "type", "type": "u64" }] } }),
    ];

    // A safe account is not laid out, but the IDL does not prove its enum's size.
    let mut safe = idl(json!({ "defined": { "name": "Repr" } }));

    if let Some(outer) = safe["types"]
        .as_array_mut()
        .and_then(|types| types.last_mut())
    {
        outer["serialization"] = json!("bytemuck");
    }

    assert!(matches!(convert(safe), Err(Error::UnknownLayout)));

    for field in unknown {
        let err = convert_at(idl(field)).expect_err("must fail");

        assert!(
            matches!(&err, Error::At { name, error, .. } if name == "Outer" && matches!(**error, Error::UnknownLayout)),
            "{err}"
        );
    }
}

#[test]
fn an_unknown_repr_kind_is_rejected() {
    let mut ty = zero_copy_c("R", json!([{ "name": "a", "type": "u8" }]));

    ty["repr"] = json!({ "kind": "other" });

    assert!(matches!(
        convert(zero_copy_idl(json!([ty]))),
        Err(Error::Json(_))
    ));
}

/// Zero-copy memory has the padding and Borsh bytes do not, so no one
/// definition reads both.
#[test]
fn a_padded_type_that_borsh_also_reads_is_rejected() {
    let rec = json!({ "defined": { "name": "Rec" } });

    let types = json!([
        { "name": "Rec", "serialization": "bytemuckunsafe", "repr": { "kind": "c" }, "type": { "kind": "struct", "fields": [
            { "name": "a", "type": "u8" },
            { "name": "b", "type": "u64" }
        ] } },
        { "name": "Holder", "serialization": "bytemuckunsafe", "repr": { "kind": "c" }, "type": { "kind": "struct", "fields": [
            { "name": "rec", "type": rec }
        ] } },
        { "name": "Acc", "type": { "kind": "struct", "fields": [{ "name": "rec", "type": rec }] } },
        { "name": "Ev", "type": { "kind": "struct", "fields": [{ "name": "rec", "type": rec }] } }
    ]);

    let idl = |args: Value, accounts: Value, events: Value| {
        let mut idl = ix(args, json!([]));

        idl["types"] = types.clone();
        idl["accounts"] = accounts;
        idl["events"] = events;
        idl
    };

    let holder = json!({ "name": "Holder", "discriminator": [1] });

    let borsh_reads = [
        idl(
            json!([{ "name": "recs", "type": { "vec": rec } }]),
            json!([holder]),
            json!([]),
        ),
        idl(
            json!([]),
            json!([holder, { "name": "Acc", "discriminator": [2] }]),
            json!([]),
        ),
        idl(
            json!([]),
            json!([holder]),
            json!([{ "name": "Ev", "discriminator": [3] }]),
        ),
    ];

    for idl in borsh_reads {
        let err = convert_at(idl).expect_err("must fail").to_string();

        assert!(err.contains("type `Rec`") && err.contains("Borsh"), "{err}");
    }

    let root = convert(idl(json!([]), json!([holder]), json!([]))).expect("converts");

    assert_eq!(
        root["program"]["definedTypes"][0]["type"]["fields"][1]["name"],
        "paddingBeforeB"
    );

    // With no zero-copy account, `Rec` is only ever Borsh bytes, which have no gap.
    let root = convert(idl(
        json!([{ "name": "recs", "type": { "vec": rec } }]),
        json!([]),
        json!([]),
    ))
    .expect("converts");

    assert_eq!(
        root["program"]["definedTypes"][0]["type"]["fields"][1]["name"],
        "b"
    );
}

/// Recomputed per use, this 60-deep chain of two-field structs would take 2^60
/// steps. The self-referencing struct must end too, in an error.
#[test]
fn nested_zero_copy_layouts_are_not_a_hang() {
    let zero_copy = |name: &str, field: Value| {
        zero_copy_c(
            name,
            json!([{ "name": "a", "type": field }, { "name": "b", "type": field }]),
        )
    };

    let types: Vec<Value> = (0..60)
        .map(|i| {
            let next = if i == 59 {
                json!("u8")
            } else {
                json!({ "defined": { "name": format!("T{}", i + 1) } })
            };

            zero_copy(&format!("T{i}"), next)
        })
        .collect();

    assert!(matches!(
        convert(zero_copy_idl(json!(types))),
        Err(Error::FixedTypeTooLarge)
    ));

    let looped = zero_copy("Loop", json!({ "defined": { "name": "Loop" } }));

    assert!(matches!(
        convert(zero_copy_idl(json!([looped]))),
        Err(Error::UnknownLayout)
    ));
}

/// An empty discriminator would match every instruction, account or event of
/// the program; the error names the item.
#[test]
fn an_empty_discriminator_is_rejected_with_its_item() {
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

/// `short` data whose next byte is 7 would read as `long`. Equal instructions are
/// told apart by account count; equal accounts would read as the first.
#[test]
fn an_ambiguous_discriminator_is_rejected() {
    let go = |name: &str, d: Value| json!({ "name": name, "discriminator": d, "accounts": [], "args": [] });

    let prefix =
        json!({ "instructions": [go("long", json!([5, 6, 7])), go("short", json!([5, 6]))] });

    assert_eq!(
        convert_at(prefix).expect_err("must fail").to_string(),
        "instruction `short`: discriminator [5, 6] is a prefix of `long`'s"
    );

    let equal_instructions =
        json!({ "instructions": [go("place", json!([1])), go("cancel", json!([1]))] });

    assert!(convert(equal_instructions).is_ok());

    let equal_accounts = json!({
        "accounts": [{ "name": "A", "discriminator": [1] }, { "name": "B", "discriminator": [1] }],
        "types": [unit("A"), unit("B")]
    });

    assert_eq!(
        convert_at(equal_accounts)
            .expect_err("must fail")
            .to_string(),
        "account `B`: same discriminator as `A`"
    );

    let across_kinds = json!({
        "instructions": [go("go", json!([1]))],
        "events": [{ "name": "E", "discriminator": [1, 2] }],
        "types": [unit("E")]
    });

    assert!(convert(across_kinds).is_ok());
}

/// The event renderer panics on any payload but a struct; JS writes it as is.
#[test]
fn an_event_that_is_not_a_struct_is_rejected() {
    let payloads = [
        json!({ "kind": "enum", "variants": [{ "name": "A" }] }),
        json!({ "kind": "struct", "fields": ["u8"] }),
    ];

    for ty in payloads {
        let idl = json!({
            "events": [{ "name": "Ev", "discriminator": [3] }],
            "types": [{ "name": "Ev", "type": ty }]
        });

        let err = convert_at(idl).expect_err("must fail");

        assert_eq!(err.to_string(), "event `Ev`: type is not a struct");
    }
}

/// JS treats an empty `address` as absent, so the PDA still gives the default.
#[test]
fn an_empty_address_falls_back_to_the_pda() {
    let seed = json!({ "kind": "const", "value": [1] });
    let account = json!([{ "name": "vault", "address": "", "pda": { "seeds": [seed] } }]);

    let root = convert(ix(json!([]), account)).expect("converts");

    assert_eq!(
        root["program"]["instructions"][0]["accounts"][0]["defaultValue"]["kind"],
        "pdaValueNode"
    );
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

/// Anchor writes an empty struct without a `fields` key.
#[test]
fn a_zero_copy_marker_struct_without_fields_is_empty() {
    let mut marker = zero_copy_c("Marker", json!([]));
    marker["type"]
        .as_object_mut()
        .expect("object")
        .remove("fields");

    let types = json!([
        zero_copy_c(
            "Acc",
            json!([
                { "name": "a", "type": "u8" },
                { "name": "m", "type": { "defined": { "name": "Marker" } } },
                { "name": "b", "type": "u64" }
            ])
        ),
        marker
    ]);

    assert_eq!(
        field_sizes(types, ACCOUNT),
        json!([["a", null], ["m", null], ["paddingBeforeB", 7], ["b", null]])
    );
}

/// A `repr(transparent)` newtype has the layout of its one field.
#[test]
fn a_transparent_newtype_in_a_zero_copy_account_has_a_layout() {
    let types = json!([
        zero_copy_c("Acc", json!([
            { "name": "a", "type": "u8" },
            { "name": "amt", "type": { "defined": { "name": "Amount" } } }
        ])),
        {
            "name": "Amount",
            "serialization": "bytemuckunsafe",
            "repr": { "kind": "transparent" },
            "type": { "kind": "struct", "fields": ["u64"] }
        }
    ]);

    assert_eq!(
        field_sizes(types, ACCOUNT),
        json!([["a", null], ["paddingBeforeAmt", 7], ["amt", null]])
    );
}

/// Anchor omits `generics` when empty; a hand-written `[]` means the same.
#[test]
fn an_explicit_empty_generics_list_is_a_plain_type() {
    let types = json!([
        { "name": "Plain", "generics": [], "type": { "kind": "struct", "fields": [{ "name": "a", "type": "u8" }] } },
        { "name": "Outer", "type": { "kind": "struct", "fields": [
            { "name": "p", "type": { "defined": { "name": "Plain", "generics": [] } } }
        ] } }
    ]);

    let root = convert(json!({ "types": types })).expect("converts");

    assert_eq!(
        root.pointer("/program/definedTypes/1/type/fields/0/type/kind"),
        Some(&json!("definedTypeLinkNode"))
    );
}

/// A typedef without a body is an empty struct for a defined type, so an
/// account reads it the same way instead of failing on `null`.
#[test]
fn an_account_typedef_without_a_body_is_an_empty_struct() {
    let root = convert(json!({
        "types": [{ "name": "Acc" }],
        "accounts": [{ "name": "Acc", "discriminator": [1] }]
    }))
    .expect("converts");

    let fields = root
        .pointer("/program/accounts/0/data/fields")
        .and_then(Value::as_array);

    assert_eq!(fields.map(Vec::len), Some(1), "only the discriminator");
}
