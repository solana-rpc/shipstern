//! Convert an Anchor IDL (spec `0.1.0`) into `codama-nodes`, matching
//! `@codama/nodes-from-anchor@1.5.6` wherever the result reaches a generated
//! parser.
//!
//! ```rust, ignore
//! let idl = serde_json::from_slice(&std::fs::read("idl.json")?)?;
//! let root = shipstern_codama_from_anchor::root_node_from_anchor(idl)?;
//! ```

use codama_nodes::RootNode;

mod case;
mod idl;
mod passes;
mod types;
mod v01;

/// The only Anchor IDL spec this crate converts.
pub const SUPPORTED_SPEC: &str = "0.1.0";

/// Deepest type nesting accepted. It equals serde_json's parser limit; real
/// IDLs stay under 20.
pub const NESTING_LIMIT: usize = 128;

/// Why an Anchor IDL could not be converted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid Anchor IDL JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error(
        "unsupported Anchor IDL spec {}: only \"{SUPPORTED_SPEC}\" (Anchor 0.30+) is supported",
        found.as_deref().map_or_else(|| "(missing metadata.spec)".to_owned(), |s| format!("{s:?}"))
    )]
    UnsupportedSpec { found: Option<String> },

    #[error("unrecognized Anchor IDL type {0}")]
    UnrecognizedType(String),

    #[error("generic type `{0}` is not defined")]
    GenericTypeMissing(String),

    #[error("generic argument {0} is not bound")]
    GenericArgMissing(String),

    #[error("invalid array length {0}")]
    InvalidArrayLength(String),

    #[error("IDL nesting exceeds the intentional limit of {NESTING_LIMIT} levels")]
    RecursionLimit,

    #[error("account `{0}` has no type definition")]
    AccountTypeMissing(String),

    #[error("account `{0}` is not a struct")]
    AccountTypeNotStruct(String),

    #[error("event `{0}` has no type definition")]
    EventTypeMissing(String),

    #[error("PDA seed argument `{0}` is not an argument of the instruction")]
    ArgumentTypeMissing(String),

    #[error("unsupported PDA seed kind")]
    SeedKindUnimplemented,

    #[error("flattening instruction `{instruction}` produces duplicate arguments {names:?}")]
    ConflictingFlattenedArguments {
        instruction: String,
        names: Vec<String>,
    },
}

/// Convert a parsed Anchor IDL, like JS `rootNodeFromAnchor`. Fails on any spec
/// but [`SUPPORTED_SPEC`].
pub fn root_node_from_anchor(idl: serde_json::Value) -> Result<RootNode, Error> {
    check_spec(&idl)?;

    let idl: idl::Idl = serde_json::from_value(idl)?;

    let mut root = v01::root_node(&idl)?;

    passes::run(&mut root.program)?;

    Ok(root)
}

/// Checked before deserializing so a legacy IDL reports its spec, not a missing
/// field. JS converts it as legacy instead; that path is out of scope here.
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
