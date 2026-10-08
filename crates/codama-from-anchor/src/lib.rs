//! Convert an Anchor IDL into `codama-nodes`.
//! See the README for differences from `@codama/nodes-from-anchor@1.5.6`.

use codama_nodes::RootNode;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

mod case;
mod idl;
mod layout;
mod passes;
mod types;
mod v01;

/// The Anchor IDL spec this crate converts. A legacy IDL is upgraded to it first.
pub const SUPPORTED_SPEC: &str = "0.1.0";

/// Deepest type nesting accepted. It equals serde_json's parser limit; real
/// IDLs stay under 20.
pub(crate) const NESTING_LIMIT: usize = 128;

/// Type nodes one IDL may expand to, generic expansion included.
pub(crate) const TYPE_NODE_LIMIT: usize = 100_000;

/// Largest fixed wire type a parser accepts.
pub(crate) const FIXED_TYPE_SIZE_LIMIT: u64 = 10 * 1024 * 1024;

/// The kind of IDL item an [`Error::At`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    Program,
    Type,
    Account,
    Constant,
    Error,
    Event,
    Instruction,
}

impl std::fmt::Display for ItemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Program => "program",
            Self::Type => "type",
            Self::Account => "account",
            Self::Constant => "constant",
            Self::Error => "error",
            Self::Event => "event",
            Self::Instruction => "instruction",
        })
    }
}

/// Why an Anchor IDL could not be converted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    // Not a `source` here or in `At`, so a report that walks causes prints it once.
    #[error("invalid Anchor IDL JSON: {0}")]
    Json(serde_json::Error),

    #[error(
        "unsupported Anchor IDL spec {}: only \"{SUPPORTED_SPEC}\" (Anchor 0.30+) or a legacy IDL (top-level `name` and `instructions`) is supported",
        found.as_deref().map_or_else(|| "(missing or not a string)".to_owned(), |s| format!("{s:?}"))
    )]
    UnsupportedSpec { found: Option<String> },

    #[error("legacy IDL is not from Anchor: {0}")]
    NotAnchor(String),

    #[error("legacy IDL has no program id in `metadata.address`")]
    MissingAddress,

    #[error("{0} in a legacy IDL is not supported")]
    LegacyUnsupported(&'static str),

    #[error("cannot upgrade legacy Anchor IDL: {0}")]
    Legacy(String),

    #[error("{kind} `{name}`: {error}")]
    At {
        kind: ItemKind,
        name: String,
        error: Box<Error>,
    },

    #[error("discriminator is empty")]
    EmptyDiscriminator,

    #[error("name {0:?} does not convert to a Rust identifier")]
    InvalidName(String),

    #[error("same Rust name as `{0}`")]
    NameCollision(String),

    #[error("discriminator {discriminator:?} is a prefix of `{other}`'s")]
    AmbiguousDiscriminator {
        discriminator: Vec<u8>,
        other: String,
    },

    #[error("same discriminator as `{0}`")]
    DuplicateDiscriminator(String),

    #[error("unrecognized Anchor IDL type {0}")]
    UnrecognizedType(String),

    #[error("generic type `{0}` is not defined")]
    GenericTypeMissing(String),

    #[error("generic type `{0}` is used without arguments")]
    GenericArgsMissing(String),

    #[error("generic enum `{0}` would expand inline, which the generated parser cannot render")]
    GenericEnum(String),

    #[error(
        "an enum written inline, not as a named type, which the generated parser cannot render"
    )]
    InlineEnum,

    #[error("coption of a type with no fixed size")]
    VariableSizeCOption,

    #[error("type `{0}` is not defined")]
    UndefinedType(String),

    #[error("generic argument {0} is not bound")]
    GenericArgMissing(String),

    #[error("invalid array length {0}")]
    InvalidArrayLength(String),

    #[error("type nesting exceeds {NESTING_LIMIT} levels")]
    RecursionLimit,

    #[error("type expansion exceeds {TYPE_NODE_LIMIT} type nodes (nested generics?)")]
    TooLarge,

    #[error("fixed-size type exceeds the 10 MiB limit")]
    FixedTypeTooLarge,

    #[error(
        "has implicit repr(C) padding but Borsh also reads it, so one of the two would be misread"
    )]
    PaddedBorshType,

    #[error("is zero_copy(unsafe), but its layout cannot be computed")]
    UnknownLayout,

    #[error("no type definition")]
    TypeMissing,

    #[error("type is not a struct")]
    TypeNotStruct,

    #[error("flattening produces duplicate arguments {0:?}")]
    ConflictingFlattenedArguments(Vec<String>),

    #[error("an argument named `discriminator` clashes with the instruction discriminator")]
    DiscriminatorArgument,
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self { Self::Json(err) }
}

impl Error {
    /// Name the IDL item the error came from.
    pub(crate) fn at(self, kind: ItemKind, name: &str) -> Self {
        Self::At {
            kind,
            name: name.to_owned(),
            error: Box::new(self),
        }
    }
}

/// Convert a parsed Anchor IDL, like JS `rootNodeFromAnchor`. A legacy IDL
/// (Anchor before 0.30) is upgraded first; any spec but [`SUPPORTED_SPEC`] fails.
pub fn root_node_from_anchor(idl: Value) -> Result<RootNode, Error> {
    let idl = upgrade_legacy(idl)?;

    check_spec(&idl)?;

    let mut idl =
        idl::Idl::deserialize(&idl).map_err(|err| idl::locate(&idl).unwrap_or(Error::Json(err)))?;

    layout::pad_zero_copy(&mut idl)?;

    let mut root = v01::root_node(&idl)?;

    passes::run(&mut root.program)?;

    Ok(root)
}

/// Upgrade a legacy IDL (top-level `name` and `instructions`, no `address` or
/// `metadata.spec`) with Anchor's converter; anything else goes to the spec check.
fn upgrade_legacy(mut idl: Value) -> Result<Value, Error> {
    let is_legacy = idl.pointer("/metadata/spec").is_none()
        && idl.get("address").is_none()
        && idl.get("name").is_some_and(Value::is_string)
        && idl.get("instructions").is_some_and(Value::is_array);

    if !is_legacy {
        return Ok(idl);
    }

    check_legacy(&idl)?;

    // The parser never reads PDAs, and the upgrade fails on any const seed that
    // is not a string or bytes.
    if let Some(ixs) = idl.get_mut("instructions").and_then(Value::as_array_mut) {
        for ix in ixs {
            if let Some(accounts) = ix.get_mut("accounts") {
                strip_pdas(accounts);
            }
        }
    }

    // Anchor 0.29 wrote a `usize` constant's type as `{"defined": "usize"}`, which no
    // IDL defines. `usize` is 64 bits on Solana.
    if let Some(constants) = idl.get_mut("constants").and_then(Value::as_array_mut) {
        for constant in constants {
            if constant.pointer("/type/defined").and_then(Value::as_str) == Some("usize") {
                constant["type"] = Value::from("u64");
            }
        }
    }

    let mut upgraded = anchor_lang_idl::convert::convert_idl(&serde_json::to_vec(&idl)?)
        .map_err(|err| locate_legacy(&idl).unwrap_or_else(|| Error::Legacy(err.to_string())))?;

    check_shadowed_fields(&upgraded)?;

    // The upgrade uses heck 0.3, which turns `setAB` (Rust `set_a_b`) into `set_ab`.
    // JS's snake case gives `set_a_b`, the name the program hashes.
    let legacy_names =
        legacy_items(&idl, "instructions").map(|ix| ix.get("name").and_then(Value::as_str));

    for (ix, legacy_name) in upgraded.instructions.iter_mut().zip(legacy_names) {
        if let Some(legacy_name) = legacy_name {
            ix.name = case::snake_case(legacy_name);
            ix.discriminator = discriminator("global", &ix.name);
        }
    }

    // Anchor hashes the Rust type name. Some hand-made IDLs lowercase its first
    // letter, like Pyth's `priceUpdateV2`, and the upgrade hashes that as written.
    for account in &mut upgraded.accounts {
        account.discriminator = discriminator("account", &upper_first(&account.name));
    }

    for event in &mut upgraded.events {
        event.discriminator = discriminator("event", &upper_first(&event.name));
    }

    Ok(serde_json::to_value(upgraded)?)
}

fn discriminator(namespace: &str, name: &str) -> Vec<u8> {
    Sha256::digest(format!("{namespace}:{name}").as_bytes())[..8].to_vec()
}

/// Not JS's pascal case, which also drops `_`: `Pool_State` is hashed as written.
fn upper_first(name: &str) -> String {
    let mut chars = name.chars();

    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

/// Refuse what the upgrade would turn into a wrong parser or drop silently.
fn check_legacy(idl: &Value) -> Result<(), Error> {
    // Shank and similar tools write this shape with their own discriminators,
    // which the upgrade would replace with Anchor's sha256 ones.
    if let Some(origin) = idl.pointer("/metadata/origin").and_then(Value::as_str)
        && origin != "anchor"
    {
        return Err(Error::NotAnchor(format!("metadata.origin is {origin:?}")));
    }

    for (section, kind) in [
        ("instructions", ItemKind::Instruction),
        ("accounts", ItemKind::Account),
        ("events", ItemKind::Event),
    ] {
        for item in legacy_items(idl, section) {
            let Some(key) = ["discriminant", "discriminator"]
                .into_iter()
                .find(|key| item.get(key).is_some())
            else {
                continue;
            };

            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();

            return Err(Error::NotAnchor(format!(
                "{kind} `{name}` has its own `{key}`"
            )));
        }
    }

    if idl
        .pointer("/metadata/address")
        .and_then(Value::as_str)
        .is_none()
    {
        return Err(Error::MissingAddress);
    }

    // Anchor's converter has no field for `state` and drops it.
    if idl.get("state").is_some_and(|state| !state.is_null()) {
        return Err(Error::LegacyUnsupported("a `state` section"));
    }

    // Anchor's converter drops type parameters but keeps their uses.
    for (section, kind) in [("types", ItemKind::Type), ("accounts", ItemKind::Account)] {
        let generic = legacy_items(idl, section).find(|ty| {
            ty.get("generics")
                .and_then(Value::as_array)
                .is_some_and(|generics| !generics.is_empty())
        });

        if let Some(ty) = generic {
            let name = ty.get("name").and_then(Value::as_str).unwrap_or_default();

            return Err(Error::LegacyUnsupported("a generic type").at(kind, name));
        }
    }

    Ok(())
}

fn legacy_items<'a>(idl: &'a Value, section: &str) -> impl Iterator<Item = &'a Value> {
    idl.get(section)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The upgrade lists account and event fields in `types` after the legacy types,
/// and the conversion reads the first match, so a different legacy type would win.
fn check_shadowed_fields(idl: &anchor_lang_idl::types::Idl) -> Result<(), Error> {
    let items = idl
        .accounts
        .iter()
        .map(|account| (ItemKind::Account, &account.name))
        .chain(
            idl.events
                .iter()
                .map(|event| (ItemKind::Event, &event.name)),
        );

    for (kind, name) in items {
        let mut definitions = Vec::new();

        for ty in idl.types.iter().filter(|ty| ty.name == *name) {
            let mut definition = serde_json::to_value(&ty.ty)?;
            strip_docs(&mut definition);
            definitions.push(definition);
        }

        if definitions.windows(2).any(|pair| pair[0] != pair[1]) {
            return Err(
                Error::LegacyUnsupported("a different type of the same name").at(kind, name),
            );
        }
    }

    Ok(())
}

/// Event fields carry no docs after the upgrade, and docs do not change the layout.
fn strip_docs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("docs");
            map.values_mut().for_each(strip_docs);
        },
        Value::Array(items) => items.iter_mut().for_each(strip_docs),
        _ => {},
    }
}

fn strip_pdas(accounts: &mut Value) {
    let Some(items) = accounts.as_array_mut() else {
        return;
    };

    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        item.remove("pda");

        if let Some(nested) = item.get_mut("accounts") {
            strip_pdas(nested);
        }
    }
}

/// Upgrade one item at a time to name the one Anchor's converter rejects.
fn locate_legacy(idl: &Value) -> Option<Error> {
    let upgrade_with = |item: Option<(&str, &Value)>| {
        let mut single = serde_json::Map::new();

        for field in ["version", "name", "metadata"] {
            if let Some(value) = idl.get(field) {
                single.insert(field.to_owned(), value.clone());
            }
        }

        single.insert("instructions".to_owned(), Value::Array(Vec::new()));

        if let Some((section, item)) = item {
            single.insert(section.to_owned(), Value::Array(vec![item.clone()]));
        }

        anchor_lang_idl::convert::convert_idl(&serde_json::to_vec(&single).ok()?).err()
    };

    // A failure with no item at all, such as a missing `version`, is the IDL's own.
    if upgrade_with(None).is_some() {
        return None;
    }

    let sections = [
        ("instructions", ItemKind::Instruction),
        ("accounts", ItemKind::Account),
        ("events", ItemKind::Event),
        ("types", ItemKind::Type),
        ("errors", ItemKind::Error),
        ("constants", ItemKind::Constant),
    ];

    sections.into_iter().find_map(|(section, kind)| {
        legacy_items(idl, section)
            .enumerate()
            .find_map(|(i, item)| {
                let err = upgrade_with(Some((section, item)))?;

                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("#{i}"), str::to_owned);

                Some(Error::Legacy(err.to_string()).at(kind, &name))
            })
    })
}

/// Checked before deserializing so an unknown spec reports itself, not a missing field.
fn check_spec(value: &Value) -> Result<(), Error> {
    let spec = value.pointer("/metadata/spec").and_then(Value::as_str);

    match spec {
        Some(SUPPORTED_SPEC) => Ok(()),
        other => Err(Error::UnsupportedSpec {
            found: other.map(str::to_owned),
        }),
    }
}
