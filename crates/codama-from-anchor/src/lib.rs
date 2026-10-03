//! Convert an Anchor IDL (spec `0.1.0`) into `codama-nodes`, matching
//! `@codama/nodes-from-anchor@1.5.6` except where the README lists a divergence.
//!
//! ```rust, ignore
//! let idl = serde_json::from_slice(&std::fs::read("idl.json")?)?;
//! let root = shipstern_codama_from_anchor::root_node_from_anchor(idl)?;
//! ```

use codama_nodes::RootNode;
use serde::Deserialize;

mod case;
mod idl;
mod layout;
mod passes;
mod types;
mod v01;

/// The only Anchor IDL spec this crate converts.
pub const SUPPORTED_SPEC: &str = "0.1.0";

/// Deepest type nesting accepted. It equals serde_json's parser limit; real
/// IDLs stay under 20.
pub(crate) const NESTING_LIMIT: usize = 128;

/// Type nodes one IDL may expand to, generic expansion included.
pub(crate) const TYPE_NODE_LIMIT: usize = 100_000;

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
        "unsupported Anchor IDL spec {}: only \"{SUPPORTED_SPEC}\" (Anchor 0.30+) is supported",
        found.as_deref().map_or_else(|| "(missing metadata.spec)".to_owned(), |s| format!("{s:?}"))
    )]
    UnsupportedSpec { found: Option<String> },

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

    #[error("unrecognized Anchor IDL type {0}")]
    UnrecognizedType(String),

    #[error("generic type `{0}` is not defined")]
    GenericTypeMissing(String),

    #[error("generic type `{0}` is used without arguments")]
    GenericArgsMissing(String),

    #[error("generic enum `{0}` would expand inline, which the generated parser cannot render")]
    GenericEnum(String),

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

    #[error(
        "has implicit repr(C) padding but Borsh also reads it, so one of the two would be misread"
    )]
    PaddedBorshType,

    #[error("is zero_copy(unsafe) repr(C), but its layout cannot be computed")]
    UnknownLayout,

    #[error("no type definition")]
    TypeMissing,

    #[error("type is not a struct")]
    AccountTypeNotStruct,

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

/// Convert a parsed Anchor IDL, like JS `rootNodeFromAnchor`. Fails on any spec
/// but [`SUPPORTED_SPEC`].
pub fn root_node_from_anchor(idl: serde_json::Value) -> Result<RootNode, Error> {
    check_spec(&idl)?;

    let mut idl =
        idl::Idl::deserialize(&idl).map_err(|err| idl::locate(&idl).unwrap_or(Error::Json(err)))?;

    layout::pad_zero_copy(&mut idl)?;

    let mut root = v01::root_node(&idl)?;

    passes::run(&mut root.program)?;

    Ok(root)
}

/// Checked before deserializing so a legacy IDL reports its spec, not a missing field.
fn check_spec(value: &serde_json::Value) -> Result<(), Error> {
    let spec = value
        .pointer("/metadata/spec")
        .and_then(serde_json::Value::as_str);

    match spec {
        Some(SUPPORTED_SPEC) => Ok(()),
        other => Err(Error::UnsupportedSpec {
            found: other.map(str::to_owned),
        }),
    }
}
