//! Anchor IDL spec `0.1.0`, mirroring `nodes-from-anchor/src/v01/idl.ts`. Type
//! positions stay `Value` so [`crate::types`] can probe keys in the JS order and
//! fail on the same shapes.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// JS `parseDocs`: a lone string is one line, and null means none.
pub(crate) fn docs<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Docs {
        One(String),
        Many(Vec<String>),
    }

    Ok(match Option::<Docs>::deserialize(de)? {
        Some(Docs::One(line)) => vec![line],
        Some(Docs::Many(lines)) => lines,
        None => Vec::new(),
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct Idl {
    pub address: String,
    pub metadata: Metadata,

    #[serde(default)]
    pub instructions: Vec<Instruction>,

    #[serde(default)]
    pub accounts: Vec<Account>,

    #[serde(default)]
    pub events: Vec<Event>,

    #[serde(default)]
    pub errors: Vec<ErrorCode>,

    #[serde(default)]
    pub types: Vec<TypeDef>,

    #[serde(default)]
    pub constants: Vec<Const>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    pub spec: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Instruction {
    pub name: String,
    pub discriminator: Vec<u8>,

    #[serde(default, deserialize_with = "docs")]
    pub docs: Vec<String>,

    #[serde(default)]
    pub accounts: Vec<InstructionAccountItem>,

    #[serde(default)]
    pub args: Vec<Field>,
}

/// A single account, or a named group of accounts (an Anchor composite).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum InstructionAccountItem {
    Group(InstructionAccounts),
    Single(InstructionAccount),
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstructionAccounts {
    pub name: String,
    pub accounts: Vec<InstructionAccountItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstructionAccount {
    pub name: String,

    #[serde(default, deserialize_with = "docs")]
    pub docs: Vec<String>,

    #[serde(default)]
    pub writable: bool,

    #[serde(default)]
    pub signer: bool,

    #[serde(default)]
    pub optional: bool,

    pub address: Option<String>,
    pub pda: Option<Pda>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pda {
    pub seeds: Vec<Seed>,
    pub program: Option<Seed>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Seed {
    Const {
        value: Vec<u8>,
    },
    Arg {
        path: String,
    },
    Account {
        path: String,
    },

    /// Rejected at conversion rather than parse time: a PDA whose seeds use
    /// nested paths is skipped before its seed kinds are looked at.
    #[serde(other)]
    Unsupported,
}

impl Seed {
    pub(crate) fn path(&self) -> Option<&str> {
        match self {
            Seed::Arg { path } | Seed::Account { path } => Some(path),
            Seed::Const { .. } | Seed::Unsupported => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub name: String,
    pub discriminator: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub name: String,
    pub discriminator: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorCode {
    pub code: u32,
    pub name: String,
    pub msg: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Field {
    pub name: String,

    #[serde(default, deserialize_with = "docs")]
    pub docs: Vec<String>,

    #[serde(rename = "type")]
    pub ty: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TypeDef {
    pub name: String,

    #[serde(default, deserialize_with = "docs")]
    pub docs: Vec<String>,

    /// Present, even when empty, marks the type as generic.
    pub generics: Option<Vec<GenericParam>>,

    #[serde(rename = "type")]
    pub ty: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenericParam {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Const {
    pub name: String,

    #[serde(rename = "type")]
    pub ty: Option<Value>,

    #[serde(default)]
    pub value: String,
}
