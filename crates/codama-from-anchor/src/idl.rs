//! Anchor IDL spec `0.1.0`, mirroring `nodes-from-anchor/src/v01/idl.ts`. Type
//! positions stay `Value` so the type conversion can probe keys in the JS order
//! and fail on the same shapes.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::{Error, ItemKind};

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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize)]
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
#[derive(Debug)]
pub enum InstructionAccountItem {
    Group(InstructionAccounts),
    Single(InstructionAccount),
}

/// Keyed on `accounts` like JS. An untagged fallback would read a malformed
/// group as one account and shift every account after it.
impl<'de> Deserialize<'de> for InstructionAccountItem {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(de)?;

        let item = if value.get("accounts").is_some() {
            serde_json::from_value(value).map(Self::Group)
        } else {
            serde_json::from_value(value).map(Self::Single)
        };

        item.map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Deserialize)]
pub struct InstructionAccounts {
    pub name: String,
    pub accounts: Vec<InstructionAccountItem>,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
pub struct Pda {
    pub seeds: Vec<Seed>,
    pub program: Option<Seed>,
}

#[derive(Debug, Deserialize)]
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

    /// A kind this crate does not model. Its PDA is dropped, not rejected.
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

#[derive(Debug, Deserialize)]
pub struct Account {
    pub name: String,
    pub discriminator: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct Event {
    pub name: String,
    pub discriminator: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorCode {
    #[serde(deserialize_with = "error_code")]
    pub code: u32,
    pub name: String,
    pub msg: Option<String>,
}

/// Some generators write `"6000"`, which the Codama JSON loader coerces too.
fn error_code<'de, D: Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Code {
        Number(u32),
        Text(String),
    }

    match Code::deserialize(de)? {
        Code::Number(code) => Ok(code),
        Code::Text(text) => text.parse().map_err(serde::de::Error::custom),
    }
}

/// JS reads a constant's value as text, so a bare number is taken as written.
fn text<'de, D: Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(de)? {
        Value::String(text) => text,
        other => other.to_string(),
    })
}

#[derive(Debug, Deserialize)]
pub struct Field {
    pub name: String,

    #[serde(default, deserialize_with = "docs")]
    pub docs: Vec<String>,

    #[serde(rename = "type")]
    pub ty: Value,
}

#[derive(Debug, Deserialize)]
pub struct TypeDef {
    pub name: String,

    #[serde(default, deserialize_with = "docs")]
    pub docs: Vec<String>,

    /// Present, even when empty, marks the type as generic.
    pub generics: Option<Vec<GenericParam>>,

    #[serde(rename = "type")]
    pub ty: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct GenericParam {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Const {
    pub name: String,

    #[serde(rename = "type")]
    pub ty: Option<Value>,

    #[serde(default, deserialize_with = "text")]
    pub value: String,
}

/// The item that broke deserialization, so the error can name it. Only called
/// after the whole IDL failed.
pub(crate) fn locate(idl: &Value) -> Option<Error> {
    fn first<'a, T: Deserialize<'a>>(idl: &'a Value, key: &str, kind: ItemKind) -> Option<Error> {
        let items = idl.get(key)?.as_array()?;

        items.iter().enumerate().find_map(|(i, item)| {
            let err = T::deserialize(item).err()?;

            let name = item
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| format!("#{i}"), str::to_owned);

            Some(Error::Json(err).at(kind, &name))
        })
    }

    first::<Instruction>(idl, "instructions", ItemKind::Instruction)
        .or_else(|| first::<Account>(idl, "accounts", ItemKind::Account))
        .or_else(|| first::<Event>(idl, "events", ItemKind::Event))
        .or_else(|| first::<TypeDef>(idl, "types", ItemKind::Type))
        .or_else(|| first::<ErrorCode>(idl, "errors", ItemKind::Error))
        .or_else(|| first::<Const>(idl, "constants", ItemKind::Constant))
}
