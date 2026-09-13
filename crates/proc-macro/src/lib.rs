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
/// Generate a Shipstern parser from a Codama JSON IDL at compile time.
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
/// The input must be Codama JSON, not a raw Anchor IDL. Event and self-CPI
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
    let (idl, events) = match parse::load_codama_idl(full_path) {
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
