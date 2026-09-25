extern crate proc_macro;

use proc_macro::TokenStream;
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, Ident, LitInt, LitStr, Token,
};

mod intermediate_representation;
mod parse;
mod render;
mod shipstern;
mod utils;

#[cfg(test)]
mod anchor_parity_tests;

/// Attribute macro that auto-infers prost annotations from Rust types.
///
/// # Modes
///
/// - `#[shipstern]` — struct with `prost::Message` (default)
/// - `#[shipstern(oneof)]` — enum with `prost::Oneof`
/// - `#[shipstern(enumeration)]` — enum with `prost::Enumeration`
///
/// Fields are auto-tagged starting at 1. Use `#[hint(...)]` on individual
/// fields when the type can't be auto-inferred.
#[proc_macro_attribute]
pub fn shipstern(attr: TokenStream, item: TokenStream) -> TokenStream {
    shipstern::expand(attr.into(), item.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

///
/// Generate a Shipstern parser from an IDL at compile time.
///
/// The path is resolved relative to the invoking crate's root
/// (`CARGO_MANIFEST_DIR`). Nothing is written to disk. The generated module is
/// named after the program and carries `PROGRAM_ID`, `InstructionParser`,
/// `AccountParser`, and the argument and account types.
///
/// ```rust, ignore
/// include_shipstern_parser!("idls/my_program.json");
/// ```
///
/// The input is Codama JSON or an Anchor IDL in spec `0.1.0` (Anchor 0.30 and
/// later), which is converted in-process. An older Anchor IDL fails to compile;
/// convert it with `codama convert` and pass the Codama JSON. Event and self-CPI
/// parsing additionally requires the `program-events` feature, which changes
/// `InstructionParser::Output` from `Instructions` to `ProgramEventOutput`.
///
/// A self-CPI event envelope declared in the IDL always wins. The optional
/// `cpi_event_discriminator` and `cpi_event_payload_offset` arguments are a
/// fallback for IDLs that declare none, and passing them alongside an
/// IDL-declared envelope emits a deprecation warning:
///
/// ```rust, ignore
/// include_shipstern_parser!(
///     "idls/custom_events.json",
///     cpi_event_discriminator = 0xfe,
///     cpi_event_payload_offset = 1,
/// );
/// ```
///
/// `docs/codama-parser-generation.md` is the reference for the envelope: how to
/// declare one in Codama, the full precedence rules, and the byte layout.
///
#[proc_macro]
pub fn include_shipstern_parser(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as IncludeShipsternParserInput);

    match input.parser_config() {
        Ok(config) => expand_include_shipstern_parser(
            input.idl_path.value(),
            config,
            input.has_cpi_event_args(),
        ),
        Err(err) => err.to_compile_error().into(),
    }
}

///
/// Parsed input of `include_shipstern_parser!`: the IDL path plus the optional
/// CPI event overrides.
///
/// The user-facing contract, including how the envelope resolves against an
/// IDL-declared one, is documented on
/// [`include_shipstern_parser`]. The overrides are applied to a default
/// [`ParserConfig`](crate::render::shipstern_parser::ParserConfig) by
/// [`Self::parser_config`], which `parse::program_envelope` may then overwrite.
///
struct IncludeShipsternParserInput {
    idl_path: LitStr,
    cpi_event_discriminator: Option<HexBytesLiteral>,
    cpi_event_payload_offset: Option<LitInt>,
}

enum HexBytesLiteral {
    Str(LitStr),
    Int(LitInt),
}

impl Parse for HexBytesLiteral {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.peek(LitStr) {
            Ok(Self::Str(input.parse()?))
        } else {
            Ok(Self::Int(input.parse()?))
        }
    }
}

impl Parse for IncludeShipsternParserInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let idl_path = input.parse()?;
        let mut cpi_event_discriminator = None;
        let mut cpi_event_payload_offset = None;

        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "cpi_event_discriminator" => {
                    cpi_event_discriminator = Some(input.parse()?);
                },
                "cpi_event_payload_offset" => {
                    cpi_event_payload_offset = Some(input.parse()?);
                },
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "unsupported include_shipstern_parser option",
                    ));
                },
            }
        }

        Ok(Self {
            idl_path,
            cpi_event_discriminator,
            cpi_event_payload_offset,
        })
    }
}

impl IncludeShipsternParserInput {
    /// Whether the call site supplied either CPI event override.
    fn has_cpi_event_args(&self) -> bool {
        self.cpi_event_discriminator.is_some() || self.cpi_event_payload_offset.is_some()
    }

    fn parser_config(&self) -> syn::Result<crate::render::shipstern_parser::ParserConfig> {
        let mut config = crate::render::shipstern_parser::ParserConfig::default();

        if let Some(discriminator) = &self.cpi_event_discriminator {
            config.cpi_event.discriminator = decode_hex_bytes_literal(discriminator)?;
        }

        if let Some(offset) = &self.cpi_event_payload_offset {
            config.cpi_event.payload_offset = offset.base10_parse()?;
        } else if self.cpi_event_discriminator.is_some() {
            config.cpi_event.payload_offset = config.cpi_event.discriminator.len();
        }

        if config.cpi_event.discriminator.is_empty() {
            return Err(syn::Error::new(
                self.idl_path.span(),
                "cpi_event_discriminator must not be empty",
            ));
        }

        if config.cpi_event.payload_offset < config.cpi_event.discriminator.len() {
            return Err(syn::Error::new(
                self.cpi_event_payload_offset
                    .as_ref()
                    .map_or_else(|| self.idl_path.span(), LitInt::span),
                "cpi_event_payload_offset must be greater than or equal to \
                 cpi_event_discriminator length",
            ));
        }

        Ok(config)
    }
}

fn decode_hex_bytes_literal(lit: &HexBytesLiteral) -> syn::Result<Vec<u8>> {
    match lit {
        HexBytesLiteral::Str(lit) => crate::utils::decode_hex_text(&lit.value())
            .map_err(|err| invalid_cpi_event_discriminator_hex(lit.span(), err)),
        HexBytesLiteral::Int(lit) => decode_int_literal(lit),
    }
}

fn decode_int_literal(lit: &LitInt) -> syn::Result<Vec<u8>> {
    let value = lit.to_string();
    let trimmed = value.trim();

    if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        return crate::utils::decode_hex_text(trimmed)
            .map_err(|err| invalid_cpi_event_discriminator_hex(lit.span(), err));
    }

    let value = lit.base10_parse::<u128>()?;
    if value == 0 {
        return Ok(vec![0]);
    }

    let bytes = value.to_be_bytes();
    let first_non_zero = bytes
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(bytes.len() - 1);
    Ok(bytes[first_non_zero..].to_vec())
}

fn invalid_cpi_event_discriminator_hex(
    span: proc_macro2::Span,
    err: hex::FromHexError,
) -> syn::Error {
    syn::Error::new(
        span,
        format!("cpi_event_discriminator must be hex bytes: {err}"),
    )
}

fn expand_include_shipstern_parser(
    idl_path: String,
    config: crate::render::shipstern_parser::ParserConfig,
    has_cpi_event_args: bool,
) -> TokenStream {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");

    let full_path = std::path::Path::new(&manifest_dir).join(&idl_path);

    expand_parser_tokens(&full_path, config, has_cpi_event_args).into()
}

/// Split from `expand_include_shipstern_parser` so unit tests can reach the
/// emission paths; a test cannot construct a `proc_macro::TokenStream`.
fn expand_parser_tokens(
    full_path: &std::path::Path,
    mut config: crate::render::shipstern_parser::ParserConfig,
    has_cpi_event_args: bool,
) -> proc_macro2::TokenStream {
    let (idl, events) = match parse::load_idl(full_path) {
        Ok(loaded) => loaded,
        Err(e) => {
            let error_msg = format!("Failed to load/parse IDL from {:?}: {}", full_path, e);

            return quote::quote! {
                compile_error!(#error_msg);
            };
        },
    };

    // The IDL wins; macro args remain a fallback for IDLs declaring no envelope.
    let envelope = match parse::program_envelope(&events) {
        Ok(envelope) => envelope,
        Err(message) => {
            let error_msg = format!("Invalid CPI event envelope in {:?}: {}", full_path, message);

            return quote::quote! {
                compile_error!(#error_msg);
            };
        },
    };

    let deprecation = match &envelope {
        Some(envelope) => {
            config.cpi_event.discriminator = envelope.envelope.discriminator.clone();
            config.cpi_event.payload_offset = envelope.envelope.payload_offset;
            config.idl_envelope = Some(envelope.clone());

            if has_cpi_event_args {
                cpi_event_args_deprecation()
            } else {
                quote::quote! {}
            }
        },

        None => quote::quote! {},
    };

    // Checked against the resolved tag, so the macro-arg fallback and the Anchor
    // default are covered too, not just IDL-declared envelopes.
    if let Some(message) = parse::envelope_instruction_collision(
        &config.cpi_event.discriminator,
        &idl.program.instructions,
    ) {
        let error_msg = format!("Invalid CPI event envelope in {:?}: {}", full_path, message);

        return quote::quote! {
            compile_error!(#error_msg);
        };
    }

    let parser = crate::render::shipstern_parser(&idl, &events, &config);

    quote::quote! {
        #deprecation
        #parser
    }
}

/// Proc macros cannot emit diagnostics on stable, so the warning rides the
/// deprecation lint instead.
fn cpi_event_args_deprecation() -> proc_macro2::TokenStream {
    quote::quote! {
        const _: () = {
            #[deprecated(
                note = "the IDL declares a CPI event envelope, so cpi_event_discriminator and \
                        cpi_event_payload_offset are ignored; prefer declaring the envelope in \
                        the IDL"
            )]
            const CPI_EVENT_ARGS_IGNORED: () = ();

            let _ = CPI_EVENT_ARGS_IGNORED;
        };
    }
}

#[cfg(test)]
mod expansion_tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/idls")
            .join(name)
    }

    fn expand(name: &str, has_cpi_event_args: bool) -> String {
        expand_parser_tokens(
            &fixture(name),
            crate::render::shipstern_parser::ParserConfig::default(),
            has_cpi_event_args,
        )
        .to_string()
    }

    /// Guards the wiring: the collision verdict is unit-tested on its own, but
    /// nothing else proves it is emitted rather than computed and dropped.
    #[test]
    fn colliding_envelope_is_emitted_as_a_compile_error() {
        let tokens = expand("colliding_envelope.json", false);

        assert!(tokens.contains("compile_error"), "no compile_error emitted");
        assert!(
            tokens.contains("collides with instruction"),
            "collision message did not reach the emitted tokens: {tokens}",
        );
    }

    /// The same IDL with a non-colliding tag must expand to a real parser.
    #[test]
    fn non_colliding_envelope_expands_normally() {
        let tokens = expand("collision_safe_cpi_event_envelope.json", false);

        assert!(
            !tokens.contains("compile_error"),
            "unexpected compile_error: {tokens}",
        );
        assert!(tokens.contains("InstructionParser"));
    }

    /// Mismatched payload offsets must also reach `compile_error!`.
    #[test]
    fn envelope_validation_errors_are_emitted() {
        let tokens = expand("mismatched_payload_offsets.json", false);

        assert!(tokens.contains("compile_error"), "no compile_error emitted");
        assert!(
            tokens.contains("different offsets"),
            "validation message did not reach the emitted tokens: {tokens}",
        );
    }

    /// The guard runs on the resolved tag, so the macro-arg fallback is covered
    /// even though no envelope is declared in the IDL.
    #[test]
    fn macro_arg_envelope_colliding_with_an_instruction_is_rejected() {
        let mut config = crate::render::shipstern_parser::ParserConfig::default();
        config.cpi_event.discriminator = vec![0x09];
        config.cpi_event.payload_offset = 1;

        let tokens =
            expand_parser_tokens(&fixture("macro_arg_envelope_collision.json"), config, true)
                .to_string();

        assert!(tokens.contains("compile_error"), "no compile_error emitted");
        assert!(
            tokens.contains("collides with instruction"),
            "collision message missing: {tokens}",
        );
    }

    /// The same IDL on the Anchor default builds clean.
    #[test]
    fn macro_arg_fixture_builds_on_the_anchor_default() {
        let tokens = expand("macro_arg_envelope_collision.json", false);

        assert!(
            !tokens.contains("compile_error"),
            "unexpected compile_error: {tokens}",
        );
    }

    /// An undecodable envelope discriminator reports, rather than panicking the
    /// macro through the shared decoder's `expect`.
    #[test]
    fn malformed_envelope_discriminator_is_reported() {
        let tokens = expand("malformed_envelope_discriminator.json", false);

        assert!(tokens.contains("compile_error"), "no compile_error emitted");
        assert!(
            tokens.contains("not valid"),
            "decode message missing: {tokens}",
        );
    }

    /// The deprecation fires only when an IDL envelope and macro args conflict.
    #[test]
    fn deprecation_fires_only_on_conflict() {
        let conflicting = expand("padded_cpi_event_envelope.json", true);
        assert!(
            conflicting.contains("CPI_EVENT_ARGS_IGNORED"),
            "IDL envelope plus macro args must warn",
        );

        let idl_only = expand("padded_cpi_event_envelope.json", false);
        assert!(
            !idl_only.contains("CPI_EVENT_ARGS_IGNORED"),
            "an IDL envelope alone must not warn",
        );

        // The supported 0.8.0 fallback: macro args with no IDL envelope.
        let args_only = expand("single_discriminator_event.json", true);
        assert!(
            !args_only.contains("CPI_EVENT_ARGS_IGNORED"),
            "macro args without an IDL envelope must not warn",
        );
    }
}

#[cfg(test)]
mod literal_width_tests {
    ///
    /// Integer suffixes that pin a generated literal to one width.
    ///
    /// A suffixed literal only compiles where that exact type is expected, so
    /// emitting one hard-codes an assumption about how wide codama happens to
    /// make a count, size or offset field.
    ///
    /// Deliberately limited to the widths a codama count, size or offset can
    /// reach. `u32` is excluded because prost field tags are `u32` by the
    /// protobuf spec and are numbered from field order, never from a node, so
    /// under the `proto` feature the generated code carries tens of thousands of
    /// legitimately suffixed `u32` literals. Including it would make this assert
    /// on something that is neither wrong nor ours to change.
    ///
    const WIDTH_SUFFIXES: [&str; 4] = ["usize", "isize", "u64", "i64"];

    ///
    /// Collect every width-suffixed integer literal in a token string.
    ///
    /// Matches a digit run followed immediately by a suffix, so a bare type
    /// mention (`const N: usize`) is not a hit while `558usize` is.
    ///
    /// Example output:
    ///
    /// ```rust, ignore
    /// assert_eq!(suffixed_literals("data . len () == 558usize"), vec!["558usize"]);
    /// assert!(suffixed_literals("const N : usize").is_empty());
    /// ```
    ///
    fn suffixed_literals(tokens: &str) -> Vec<String> {
        let chars: Vec<char> = tokens.chars().collect();
        let mut found = Vec::new();
        let mut i = 0;

        while i < chars.len() {
            if !chars[i].is_ascii_digit() {
                i += 1;
                continue;
            }

            // A digit run only starts a literal if nothing identifier-like precedes it.
            let starts_literal = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');

            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
                i += 1;
            }

            if !starts_literal {
                continue;
            }

            let tail: String = chars[i..(i + 6).min(chars.len())].iter().collect();

            if let Some(suffix) = WIDTH_SUFFIXES.iter().find(|s| tail.starts_with(**s)) {
                let digits: String = chars[start..i].iter().collect();
                found.push(format!("{digits}{suffix}"));
            }
        }

        found
    }

    fn idl_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/idls")
    }

    ///
    /// No generated parser may contain a width-pinned integer literal.
    ///
    /// Counts, sizes and offsets reach `quote!` as plain integers, and bare
    /// interpolation renders those *suffixed*: `558usize` today, `558u64` the
    /// day codama widens the field. The generated code indexes `data` and
    /// declares `usize` consts, so the suffixed form stops compiling on that
    /// bump even though the IDL never changed.
    ///
    /// Asserting over the whole fixture corpus is what makes this a guard
    /// rather than a spot check: a new interpolation site added anywhere in the
    /// renderer fails here the first time a fixture exercises it.
    ///
    #[test]
    fn generated_parsers_contain_no_width_pinned_literals() {
        let mut offenders: Vec<String> = Vec::new();
        let mut expanded = 0_usize;

        for entry in std::fs::read_dir(idl_dir()).expect("tests/idls is readable") {
            let path = entry.expect("readable dir entry").path();

            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            let tokens = super::expand_parser_tokens(
                &path,
                crate::render::shipstern_parser::ParserConfig::default(),
                false,
            )
            .to_string();

            expanded += 1;

            for literal in suffixed_literals(&tokens) {
                offenders.push(format!("{name}: {literal}"));
            }
        }

        assert!(
            expanded > 0,
            "no fixtures expanded; the corpus path is wrong"
        );

        offenders.sort();
        offenders.dedup();

        assert!(
            offenders.is_empty(),
            "{} width-pinned literal(s) in generated output across {expanded} IDLs:\n{}",
            offenders.len(),
            offenders.join("\n"),
        );
    }

    #[test]
    fn scanner_separates_literals_from_type_mentions() {
        assert_eq!(suffixed_literals("data . len () == 558usize"), ["558usize"]);
        assert_eq!(suffixed_literals("data . get (0usize .. 8usize)"), [
            "0usize", "8usize"
        ]);

        // Bare type mentions and already-unsuffixed literals are not hits.
        assert!(suffixed_literals("const N : usize , R : Read").is_empty());
        assert!(suffixed_literals("data . len () == 558").is_empty());

        // An identifier ending in digits is not a literal.
        assert!(suffixed_literals("let swap_v2usize = 1 ;").is_empty());
    }
}

#[cfg(test)]
mod field_presence_tests {
    use codama_nodes::{InstructionAccountNode, OptionTypeNode};

    ///
    /// An IDL with one optional-typed account field and one optional instruction
    /// account, where `fixed` and `isOptional` are spliced in per test case.
    ///
    /// `flags` is inserted verbatim into both nodes, so passing `""` omits the
    /// field entirely and `"\"fixed\": false,"` states it explicitly.
    ///
    fn idl_json(option_flag: &str, account_flag: &str) -> String {
        format!(
            r#"{{
  "kind": "rootNode",
  "standard": "codama",
  "version": "1.6.0",
  "program": {{
    "kind": "programNode",
    "name": "presenceMatrix",
    "publicKey": "11111111111111111111111111111111",
    "version": "0.1.0",
    "origin": "shank",
    "accounts": [
      {{
        "kind": "accountNode",
        "name": "holder",
        "data": {{
          "kind": "structTypeNode",
          "fields": [
            {{
              "kind": "structFieldTypeNode",
              "name": "discriminator",
              "type": {{ "kind": "fixedSizeTypeNode", "size": 1,
                         "type": {{ "kind": "bytesTypeNode" }} }},
              "defaultValue": {{ "kind": "bytesValueNode", "data": "07", "encoding": "base16" }}
            }},
            {{
              "kind": "structFieldTypeNode",
              "name": "maybeAmount",
              "type": {{
                "kind": "optionTypeNode",
                {option_flag}
                "item": {{ "kind": "numberTypeNode", "format": "u64", "endian": "le" }},
                "prefix": {{ "kind": "numberTypeNode", "format": "u8", "endian": "le" }}
              }}
            }},
            {{
              "kind": "structFieldTypeNode",
              "name": "tail",
              "type": {{ "kind": "numberTypeNode", "format": "u32", "endian": "le" }}
            }}
          ]
        }},
        "discriminators": [
          {{ "kind": "fieldDiscriminatorNode", "name": "discriminator", "offset": 0 }}
        ]
      }}
    ],
    "instructions": [
      {{
        "kind": "instructionNode",
        "name": "touch",
        "accounts": [
          {{ "kind": "instructionAccountNode", "name": "payer",
             "isWritable": true, "isSigner": true }},
          {{ "kind": "instructionAccountNode", "name": "maybeDelegate",
             "isWritable": false, "isSigner": false,
             {account_flag}
             "docs": [] }}
        ],
        "arguments": [
          {{
            "kind": "instructionArgumentNode",
            "name": "discriminator",
            "defaultValueStrategy": "omitted",
            "docs": [],
            "type": {{ "kind": "fixedSizeTypeNode", "size": 1,
                       "type": {{ "kind": "bytesTypeNode" }} }},
            "defaultValue": {{ "kind": "bytesValueNode", "data": "07", "encoding": "base16" }}
          }}
        ],
        "discriminators": [
          {{ "kind": "fieldDiscriminatorNode", "name": "discriminator", "offset": 0 }}
        ]
      }}
    ],
    "definedTypes": [],
    "errors": [],
    "constants": []
  }},
  "additionalPrograms": []
}}"#
        )
    }

    fn expand_with(option_flag: &str, account_flag: &str) -> String {
        // One directory per call. Keying the name on the flag lengths stopped being
        // injective once a flag reached 31 characters, and libtest runs these cases on
        // threads of one process, so two colliding cases shared a file and the equality
        // assertion passed without proving anything.
        static CASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        let dir = std::env::temp_dir().join(format!(
            "shipstern-presence-{}-{}",
            std::process::id(),
            CASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));

        std::fs::create_dir_all(&dir).expect("temp dir");

        let path = dir.join("presence.json");
        std::fs::write(&path, idl_json(option_flag, account_flag)).expect("write fixture");

        super::expand_parser_tokens(
            &path,
            crate::render::shipstern_parser::ParserConfig::default(),
            false,
        )
        .to_string()
    }

    ///
    /// An absent `fixed` must deserialize the same as an explicit `false`.
    ///
    /// codama 0.9 types the field `bool` with `#[serde(default)]`, so absent is
    /// `false`. codama 0.13 retypes it `Option<bool>`, where absent is `None` and
    /// the migration shim reads it as `.unwrap_or(false)`. This pins the 0.9 half
    /// of that equality: if absent ever stopped meaning `false` here, the shim
    /// would silently change every layout that omits the flag.
    ///
    #[test]
    fn absent_fixed_deserializes_as_false() {
        let absent: OptionTypeNode = serde_json::from_str(
            r#"{"kind":"optionTypeNode",
                "item":{"kind":"numberTypeNode","format":"u64","endian":"le"},
                "prefix":{"kind":"numberTypeNode","format":"u8","endian":"le"}}"#,
        )
        .expect("absent fixed parses");

        let explicit_false: OptionTypeNode = serde_json::from_str(
            r#"{"kind":"optionTypeNode","fixed":false,
                "item":{"kind":"numberTypeNode","format":"u64","endian":"le"},
                "prefix":{"kind":"numberTypeNode","format":"u8","endian":"le"}}"#,
        )
        .expect("explicit false parses");

        let explicit_true: OptionTypeNode = serde_json::from_str(
            r#"{"kind":"optionTypeNode","fixed":true,
                "item":{"kind":"numberTypeNode","format":"u64","endian":"le"},
                "prefix":{"kind":"numberTypeNode","format":"u8","endian":"le"}}"#,
        )
        .expect("explicit true parses");

        // codama 0.13 types this `Option<bool>` with no serde default, so absent is
        // `None`. The shim reads it as `.unwrap_or(false)`, which has to land on the
        // value codama 0.9 produced for an absent field: `false`.
        assert_eq!(absent.fixed, None);
        assert!(!absent.fixed.unwrap_or(false));

        assert_eq!(
            absent.fixed.unwrap_or(false),
            explicit_false.fixed.unwrap_or(false),
            "absent must read as false, the way codama 0.9 deserialized it",
        );

        assert!(
            explicit_true.fixed.unwrap_or(false),
            "true must stay distinguishable from absent",
        );
    }

    /// Same equality for the instruction-account flag.
    #[test]
    fn absent_is_optional_deserializes_as_false() {
        let absent: InstructionAccountNode = serde_json::from_str(
            r#"{"kind":"instructionAccountNode","name":"a","isWritable":false,"isSigner":false}"#,
        )
        .expect("absent isOptional parses");

        let explicit_false: InstructionAccountNode = serde_json::from_str(
            r#"{"kind":"instructionAccountNode","name":"a","isWritable":false,
                "isSigner":false,"isOptional":false}"#,
        )
        .expect("explicit false parses");

        let explicit_true: InstructionAccountNode = serde_json::from_str(
            r#"{"kind":"instructionAccountNode","name":"a","isWritable":false,
                "isSigner":false,"isOptional":true}"#,
        )
        .expect("explicit true parses");

        // codama 0.13 types this `Option<bool>` with no serde default, so absent is
        // `None`. The shim reads it as `.unwrap_or(false)`, which has to land on the
        // value codama 0.9 produced for an absent field: `false`.
        assert_eq!(absent.is_optional, None);
        assert!(!absent.is_optional.unwrap_or(false));

        assert_eq!(
            absent.is_optional.unwrap_or(false),
            explicit_false.is_optional.unwrap_or(false),
            "absent must read as false, the way codama 0.9 deserialized it",
        );

        assert!(
            explicit_true.is_optional.unwrap_or(false),
            "true must stay distinguishable from absent",
        );
    }

    ///
    /// The presence distinction has to survive all the way to the emitted parser,
    /// not just to the node.
    ///
    /// Deserializing absent as `false` is only meaningful if the renderer then
    /// lays the bytes out identically. Comparing whole token streams covers the
    /// layout decisions that read these flags — the fixed-option padding branch
    /// and the optional-account branch — without asserting on their internals.
    ///
    #[test]
    fn absent_flags_generate_the_same_parser_as_explicit_false() {
        let absent = expand_with("", "");
        let explicit_false = expand_with(r#""fixed": false,"#, r#""isOptional": false,"#);

        assert_eq!(
            absent, explicit_false,
            "omitting fixed/isOptional must render exactly like stating them false",
        );
    }

    ///
    /// The guard on the test above: an explicit `true` must render differently,
    /// or the comparison would pass for a renderer that ignores the flags.
    ///
    #[test]
    fn explicit_true_flags_generate_a_different_parser() {
        let absent = expand_with("", "");
        let fixed_true = expand_with(r#""fixed": true,"#, "");
        let optional_true = expand_with("", r#""isOptional": true,"#);

        assert_ne!(absent, fixed_true, "fixed: true must change the layout");
        assert_ne!(
            absent, optional_true,
            "isOptional: true must change the accounts"
        );
    }
}

#[cfg(test)]
mod discriminator_injectivity_tests {
    use crate::render::instruction_parser::{extract_ix_discriminator_key, DiscriminatorKey};

    ///
    /// Can a single buffer satisfy both discriminators at once?
    ///
    /// Each key is a constraint of the form "these bytes appear at this offset",
    /// or, for a size discriminator, "the buffer is exactly this long". Two
    /// constraints are jointly satisfiable unless they disagree somewhere they
    /// overlap, so the check is a byte-wise comparison over the intersection of
    /// the two windows.
    ///
    /// Non-overlapping windows are jointly satisfiable, which is the interesting
    /// case: two discriminators at different offsets never contradict each other,
    /// so one buffer can match both and the emitted arm order silently decides
    /// which instruction wins.
    ///
    fn jointly_satisfiable(a: &DiscriminatorKey, b: &DiscriminatorKey) -> bool {
        match (a.to_bytes_offset(), b.to_bytes_offset()) {
            (Some((a_bytes, a_off)), Some((b_bytes, b_off))) => {
                let disagrees = a_bytes.iter().enumerate().any(|(i, byte)| {
                    let pos = a_off + i;

                    pos >= b_off && pos < b_off + b_bytes.len() && *byte != b_bytes[pos - b_off]
                });

                !disagrees
            },

            // A size discriminator fixes the total length, so it can only coexist
            // with a byte window that fits inside it.
            (None, Some((bytes, off))) | (Some((bytes, off)), None) => {
                let DiscriminatorKey::Size { size } = (match a {
                    DiscriminatorKey::Size { .. } => a,
                    _ => b,
                }) else {
                    return true;
                };

                *size >= off + bytes.len()
            },

            // Two size discriminators: distinct keys mean distinct lengths.
            (None, None) => false,
        }
    }

    struct Aliasing {
        same_key: usize,
        cross_offset: Vec<String>,
    }

    fn analyse(keys: &[(String, DiscriminatorKey)]) -> Aliasing {
        let mut same_key = 0;
        let mut cross_offset = Vec::new();

        for (i, (left_name, left)) in keys.iter().enumerate() {
            for (right_name, right) in &keys[i + 1..] {
                if left == right {
                    same_key += 1;

                    continue;
                }

                if jointly_satisfiable(left, right) {
                    cross_offset.push(format!("{left_name} ~ {right_name}"));
                }
            }
        }

        Aliasing {
            same_key,
            cross_offset,
        }
    }

    ///
    /// No two *differently keyed* instructions may both match one buffer.
    ///
    /// Equal keys are a separate, handled case: the renderer groups them and
    /// emits `collision_group_match_arm`, which disambiguates on account count.
    /// Unequal keys get one arm each, tried in order, so an overlap there is
    /// decided by emission order rather than by anything the IDL states. That is
    /// the aliasing worth proving absent, and this proves it exhaustively over
    /// the corpus rather than up to a bound.
    ///
    /// One fixture from `tests/idls`, with the discriminator key of each instruction.
    struct Fixture {
        name: String,
        keys: Vec<(String, DiscriminatorKey)>,
    }

    ///
    /// Every fixture in `tests/idls`, plus the ones that would not load.
    ///
    /// Both corpus tests below walk the same directory and pull the same keys out of
    /// it; only what they do with the result differs. A fixture that stops loading is
    /// returned rather than skipped, because both tests read the whole corpus and a
    /// silent skip would let most of it drop out while they stayed green.
    ///
    fn corpus() -> (Vec<Fixture>, Vec<String>) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/idls");

        let mut fixtures = Vec::new();
        let mut unreadable = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("tests/idls is readable") {
            let path = entry.expect("readable dir entry").path();

            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let (root, _) = match crate::parse::load_idl(&path) {
                Ok(loaded) => loaded,
                Err(err) => {
                    unreadable.push(format!("{}: {err}", path.display()));
                    continue;
                },
            };

            let keys: Vec<(String, DiscriminatorKey)> = root
                .program
                .instructions
                .iter()
                .filter_map(|ix| {
                    extract_ix_discriminator_key(ix).map(|key| (ix.name.to_string(), key))
                })
                .collect();

            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            fixtures.push(Fixture { name, keys });
        }

        (fixtures, unreadable)
    }

    #[test]
    fn no_instruction_discriminator_aliases_another() {
        let (fixtures, unreadable) = corpus();

        let mut offenders = Vec::new();
        let mut compared = 0;

        for fixture in &fixtures {
            compared += fixture.keys.len();

            for pair in analyse(&fixture.keys).cross_offset {
                offenders.push(format!("{}: {pair}", fixture.name));
            }
        }

        assert!(
            unreadable.is_empty(),
            "fixtures stopped loading: {unreadable:#?}"
        );
        assert!(!fixtures.is_empty() && compared > 0, "corpus did not load");

        offenders.sort();

        assert!(
            offenders.is_empty(),
            "{} instruction discriminator pair(s) can match the same buffer:\n{}",
            offenders.len(),
            offenders.join("\n"),
        );
    }

    ///
    /// The check above is only meaningful if `jointly_satisfiable` can say yes.
    ///
    /// A helper that always returned `false` would make the corpus test pass on
    /// any input, so these pin both answers on hand-built keys.
    ///
    #[test]
    fn joint_satisfiability_detects_real_overlap() {
        let at = |off: usize, bytes: &[u8]| DiscriminatorKey::Field {
            offset: off,
            bytes: bytes.to_vec(),
        };

        // Same window, different bytes: nothing matches both.
        assert!(!jointly_satisfiable(&at(0, &[0x01]), &at(0, &[0x02])));

        // Disjoint windows: `01 .. .. .. .. .. .. .. AA` matches both.
        assert!(jointly_satisfiable(&at(0, &[0x01]), &at(8, &[0xaa])));

        // Overlapping and agreeing on the shared byte.
        assert!(jointly_satisfiable(
            &at(0, &[0x01, 0x02]),
            &at(1, &[0x02, 0x03])
        ));

        // Overlapping and disagreeing on the shared byte.
        assert!(!jointly_satisfiable(&at(0, &[0x01, 0x02]), &at(1, &[0x99])));

        // A size discriminator cannot coexist with a window past its end.
        assert!(!jointly_satisfiable(
            &DiscriminatorKey::Size { size: 4 },
            &at(8, &[0xaa])
        ));
        assert!(jointly_satisfiable(
            &DiscriminatorKey::Size { size: 16 },
            &at(8, &[0xaa])
        ));
    }

    ///
    /// Same-key groups exist and are expected. Pinning the count means a new one
    /// shows up as a deliberate change rather than passing unnoticed.
    ///
    #[test]
    fn same_key_collision_groups_are_accounted_for() {
        let (fixtures, unreadable) = corpus();

        let mut with_groups: Vec<&str> = fixtures
            .iter()
            .filter(|fixture| analyse(&fixture.keys).same_key > 0)
            .map(|fixture| fixture.name.as_str())
            .collect();

        with_groups.sort_unstable();

        assert!(
            unreadable.is_empty(),
            "fixtures stopped loading: {unreadable:#?}"
        );

        assert_eq!(
            with_groups,
            [
                "colliding_envelope",
                "collision_safe_cpi_event_envelope",
                "macro_arg_envelope_collision",
                "raydium_amm_v4_with_swapv2",
            ],
            "the set of IDLs relying on account-count disambiguation changed",
        );
    }
}
