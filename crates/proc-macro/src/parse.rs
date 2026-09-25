use std::{fs, path::Path};

use codama_nodes::{EventNode, RootNode};

#[derive(Debug)]
pub enum IdlError {
    ReadFile(std::io::Error),
    ParseFile(serde_json::Error),
    ConvertAnchor(codama_nodes_from_anchor::Error),
}

impl std::fmt::Display for IdlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdlError::ReadFile(e) => write!(f, "Failed to read file: {}", e),
            IdlError::ParseFile(e) => write!(f, "Failed to parse JSON: {}", e),
            IdlError::ConvertAnchor(
                e @ codama_nodes_from_anchor::Error::UnsupportedSpec { .. },
            ) => {
                write!(
                    f,
                    "{e}. Convert an older IDL with `npx -p codama -p @codama/nodes-from-anchor \
                     codama convert` and pass the Codama JSON instead"
                )
            },
            IdlError::ConvertAnchor(e) => write!(f, "Failed to convert Anchor IDL: {}", e),
        }
    }
}

/// Load a Codama JSON file or an Anchor IDL, returning the root node and any
/// events. Codama JSON always has a top-level `kind`; anything else is read as
/// an Anchor IDL.
pub fn load_idl<P: AsRef<Path>>(path: P) -> Result<(RootNode, Vec<EventNode>), IdlError> {
    let data = fs::read_to_string(&path).map_err(IdlError::ReadFile)?;

    let mut value: serde_json::Value = serde_json::from_str(&data).map_err(IdlError::ParseFile)?;

    let root = if value.get("kind").is_some() {
        // Some codama generators emit error codes as strings ("6000") instead of
        // integers (6000). Coerce them before deserialization so `ErrorNode.code`
        // (which expects `usize`) doesn't fail.
        fix_string_error_codes(&mut value);

        serde_json::from_value::<RootNode>(value).map_err(IdlError::ParseFile)?
    } else {
        codama_nodes_from_anchor::root_node_from_anchor(data.as_bytes())
            .map_err(IdlError::ConvertAnchor)?
    };

    let events = root.program.events.clone();

    Ok((root, events))
}

/// Coerce `"code": "6000"` → `"code": 6000` in `program.errors[]`.
fn fix_string_error_codes(value: &mut serde_json::Value) {
    let errors = value
        .get_mut("program")
        .and_then(|p| p.get_mut("errors"))
        .and_then(|e| e.as_array_mut());

    let Some(errors) = errors else { return };

    for error in errors {
        if let Some(code) = error.get_mut("code")
            && let Some(s) = code.as_str().and_then(|s| s.parse::<u64>().ok())
        {
            *code = serde_json::Value::Number(s.into());
        }
    }
}

///
/// The self-CPI event envelope declared by an IDL.
///
/// `emit_cpi!` data is `[envelope tag][event discriminator][borsh payload]`.
/// Codama has no node for the tag, so it is inferred positionally from the
/// discriminator chain, the same rule Carbon's renderer uses.
///
/// Example input:
///
/// ```rust, ignore
/// // discriminators: [const e445a52e51cb9a1d @0, const 40c6cde8260871e2 @8]
/// //   -> discriminator = e445a52e51cb9a1d, payload_offset = 8
/// ```
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpiEventEnvelope {
    /// Envelope tag bytes, always at offset 0 of the inner instruction data.
    pub discriminator: Vec<u8>,

    /// Byte offset at which the event's own discriminator begins.
    ///
    /// The declared offset of the next discriminator, not the tag length, so
    /// padded layouts round-trip.
    pub payload_offset: usize,
}

/// Decode without the shared helper's `expect`, so a malformed envelope
/// discriminator reaches `compile_error!` instead of panicking the macro.
fn try_decode_bytes(bytes: &codama_nodes::BytesValueNode) -> Option<Vec<u8>> {
    match bytes.encoding {
        codama_nodes::BytesEncoding::Base16 => hex::decode(crate::utils::pad_hex(&bytes.data)).ok(),
        codama_nodes::BytesEncoding::Base58 => bs58::decode(&bytes.data).into_vec().ok(),
        codama_nodes::BytesEncoding::Base64 => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(&bytes.data)
                .ok()
        },
        codama_nodes::BytesEncoding::Utf8 => Some(bytes.data.as_bytes().to_vec()),
    }
}

/// Numeric constants narrow to one byte, matching the generated match arm's
/// `*d == (value as u8)`.
fn constant_discriminator_bytes(
    node: &codama_nodes::ConstantDiscriminatorNode,
) -> Result<Vec<u8>, String> {
    match node.constant.value.as_ref() {
        codama_nodes::ValueNode::Bytes(bytes) => try_decode_bytes(bytes).ok_or_else(|| {
            format!(
                "discriminator at offset {} is not valid {:?} data",
                node.offset, bytes.encoding,
            )
        }),

        codama_nodes::ValueNode::Number(number) => {
            let codama_nodes::Number::UnsignedInteger(value) = number.number else {
                return Err(format!(
                    "discriminator at offset {} is a signed or floating-point number",
                    node.offset,
                ));
            };

            Ok(vec![value as u8])
        },

        _ => Err(format!(
            "discriminator at offset {} is neither bytes nor an unsigned number",
            node.offset,
        )),
    }
}

///
/// Infer the CPI event envelope declared by a single event, if any.
///
/// `Ok(None)` means no envelope: a single discriminator, or any non-constant
/// one in the chain. Those events keep their existing meaning and stay
/// matchable from `Program data:` log lines, which carry no envelope.
///
/// `Err` is reserved for chains that look enveloped but cannot be honored, so
/// an ambiguous IDL fails the build rather than parsing arbitrarily.
///
pub fn envelope_of(event: &codama_nodes::EventNode) -> Result<Option<CpiEventEnvelope>, String> {
    let mut constants = Vec::with_capacity(event.discriminators.len());

    for discriminator in &event.discriminators {
        let codama_nodes::DiscriminatorNode::Constant(node) = discriminator else {
            continue;
        };

        constants.push(node);
    }

    let leads_with_constant = constants.iter().any(|node| node.offset == 0);

    // A chain that opens with a tag but mixes in a non-constant node cannot be
    // rebased, and treating it as unenveloped would match the tag against
    // already-stripped data, so the event would silently never fire.
    if constants.len() != event.discriminators.len() {
        if event.discriminators.len() >= 2 && leads_with_constant {
            return Err(format!(
                "event `{}` mixes constant and non-constant discriminators behind a constant at \
                 offset 0; a CPI event envelope needs an all-constant chain",
                *event.name,
            ));
        }

        return Ok(None);
    }

    if constants.len() < 2 {
        return Ok(None);
    }

    {
        let at_zero = constants.iter().filter(|node| node.offset == 0).count();

        if at_zero == 0 {
            return Ok(None);
        }

        // Nothing distinguishes envelope from event discriminator here, and
        // declaration order would decide it silently.
        if at_zero > 1 {
            return Err(format!(
                "event `{}` declares {at_zero} discriminators at offset 0; a CPI event envelope \
                 needs exactly one",
                *event.name,
            ));
        }
    }

    let mut sorted = constants;
    sorted.sort_by_key(|node| node.offset);

    let mut decoded = Vec::with_capacity(sorted.len());

    for node in &sorted {
        let bytes = constant_discriminator_bytes(node)
            .map_err(|reason| format!("event `{}` has a {reason}", *event.name))?;

        decoded.push((crate::utils::as_index(node.offset), bytes));
    }

    // Overlap makes at least one arm unmatchable. Gaps are allowed: padded
    // envelopes are a real layout.
    for window in decoded.windows(2) {
        let (offset, bytes) = &window[0];
        let (next_offset, _) = &window[1];

        if offset + bytes.len() > *next_offset {
            return Err(format!(
                "event `{}` has overlapping discriminators: {} bytes at offset {offset} run past \
                 offset {next_offset}",
                *event.name,
                bytes.len(),
            ));
        }
    }

    let (_, discriminator) = decoded[0].clone();
    let (payload_offset, _) = decoded[1];

    Ok(Some(CpiEventEnvelope {
        discriminator,
        payload_offset,
    }))
}

///
/// Infer the program-wide CPI event envelope from its events.
///
/// The envelope belongs to the program, so every event declaring one must
/// agree. Events declaring none are exempt: a program may mix `emit_cpi!` and
/// `emit!` events, and rejecting that would fail programs that parse fine.
/// Carbon instead bails on the whole program in that case.
///
pub fn program_envelope(
    events: &[codama_nodes::EventNode],
) -> Result<Option<ProgramEnvelope>, String> {
    let mut found: Option<(String, CpiEventEnvelope)> = None;
    let mut enveloped = Vec::with_capacity(events.len());
    let mut lone_constants: Vec<(String, Vec<u8>)> = Vec::new();

    for event in events {
        let Some(envelope) = envelope_of(event)? else {
            enveloped.push((event.name.to_string(), false));

            if let [codama_nodes::DiscriminatorNode::Constant(node)] = &event.discriminators[..]
                && node.offset == 0
                && let Ok(bytes) = constant_discriminator_bytes(node)
            {
                lone_constants.push((event.name.to_string(), bytes));
            }

            continue;
        };

        enveloped.push((event.name.to_string(), true));

        let Some((first_name, first)) = &found else {
            found = Some((event.name.to_string(), envelope));
            continue;
        };

        if first.discriminator != envelope.discriminator {
            return Err(format!(
                "events `{}` and `{}` declare different CPI event envelopes ({} vs {}); one \
                 program has one envelope",
                first_name,
                *event.name,
                hex::encode(&first.discriminator),
                hex::encode(&envelope.discriminator),
            ));
        }

        // The parser strips one `payload_offset` for every event, so events
        // padding differently behind the same tag would decode off by the
        // difference.
        if first.payload_offset != envelope.payload_offset {
            return Err(format!(
                "events `{}` and `{}` share a CPI event envelope but start their payloads at \
                 different offsets ({} vs {}); one program has one payload offset",
                first_name, *event.name, first.payload_offset, envelope.payload_offset,
            ));
        }
    }

    let Some((_, envelope)) = found else {
        return Ok(None);
    };

    // A lone discriminator equal to the tag is the envelope with no event
    // discriminator behind it. It would be matched against already-stripped
    // data and never fire.
    for (name, bytes) in lone_constants {
        if bytes == envelope.discriminator {
            return Err(format!(
                "event `{name}` declares only the CPI event envelope {} as its discriminator, so \
                 it has none of its own",
                hex::encode(&envelope.discriminator),
            ));
        }
    }

    Ok(Some(ProgramEnvelope {
        envelope,
        enveloped,
    }))
}

///
/// A program's CPI event envelope plus which events declare it.
///
/// Both come from the same validated pass, so codegen never re-decides whether
/// an event is enveloped; it looks the answer up by index.
///
#[derive(Debug, Clone)]
pub struct ProgramEnvelope {
    /// The envelope every declaring event agrees on.
    pub envelope: CpiEventEnvelope,

    /// Per-event verdict, keyed by name and positionally aligned with the
    /// events slice.
    enveloped: Vec<(String, bool)>,
}

impl ProgramEnvelope {
    ///
    /// Resolve every event's envelope against the slice the verdicts were built
    /// from.
    ///
    /// Panics, surfacing as a build error, when the slice does not match the one
    /// validated: the verdicts are positional, so a caller that filtered or
    /// reordered events would hand each event its neighbour's answer.
    ///
    pub fn resolve<'a>(&'a self, events: &[EventNode]) -> Vec<Option<&'a CpiEventEnvelope>> {
        assert_eq!(
            events.len(),
            self.enveloped.len(),
            "CPI event envelope verdicts were built for {} events but resolved against {}; the \
             events slice changed between validation and codegen",
            self.enveloped.len(),
            events.len(),
        );

        self.enveloped
            .iter()
            .zip(events)
            .map(|((name, enveloped), event)| {
                assert_eq!(
                    name.as_str(),
                    &**event.name,
                    "CPI event envelope verdicts are out of order; expected `{name}`",
                );

                enveloped.then_some(&self.envelope)
            })
            .collect()
    }
}

///
/// The instruction an envelope tag would mask, if any.
///
/// The parser filters instructions whose data starts with the tag before it
/// dispatches, so one sharing a prefix with the tag at offset 0 can never be
/// parsed. Anchor's dispatcher checks the sentinel after every instruction, so
/// this drops real instructions rather than being a theoretical clash.
///
/// A collision is either sequence prefixing the other at offset 0, equality
/// included. Only offset 0 counts, because that is what `starts_with` compares.
///
pub fn envelope_instruction_collision(
    discriminator: &[u8],
    instructions: &[codama_nodes::InstructionNode],
) -> Option<String> {
    for instruction in instructions {
        let Some(key) =
            crate::render::instruction_parser::extract_ix_discriminator_key(instruction)
        else {
            continue;
        };

        let Some((bytes, offset)) = key.to_bytes_offset() else {
            continue;
        };

        if offset != 0 || bytes.is_empty() {
            continue;
        }

        let shared = bytes.len().min(discriminator.len());

        if bytes[..shared] != discriminator[..shared] {
            continue;
        }

        return Some(format!(
            "CPI event envelope {} collides with instruction `{}` (discriminator {} at offset 0); \
             the generated parser filters every instruction whose data starts with the envelope \
             tag, so `{}` would never parse",
            hex::encode(discriminator),
            *instruction.name,
            hex::encode(&bytes),
            *instruction.name,
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, discriminators: &str) -> EventNode {
        let json = format!(
            r#"{{"kind":"eventNode","name":"{name}","data":{{"kind":"structTypeNode","fields":[]}},
                "discriminators":[{discriminators}]}}"#
        );

        serde_json::from_str(&json).expect("event fixture")
    }

    fn constant(hex: &str, offset: usize) -> String {
        let size = hex.len() / 2;

        format!(
            r#"{{"kind":"constantDiscriminatorNode","offset":{offset},"constant":{{
                "kind":"constantValueNode",
                "type":{{"kind":"fixedSizeTypeNode","size":{size},"type":{{"kind":"bytesTypeNode"}}}},
                "value":{{"kind":"bytesValueNode","data":"{hex}","encoding":"base16"}}}}}}"#
        )
    }

    fn instruction(name: &str, hex: &str, offset: usize) -> codama_nodes::InstructionNode {
        let size = hex.len() / 2;

        let json = format!(
            r#"{{"kind":"instructionNode","name":"{name}","accounts":[],
                "arguments":[{{"kind":"instructionArgumentNode","name":"discriminator",
                    "defaultValueStrategy":"omitted",
                    "type":{{"kind":"fixedSizeTypeNode","size":{size},"type":{{"kind":"bytesTypeNode"}}}},
                    "defaultValue":{{"kind":"bytesValueNode","data":"{hex}","encoding":"base16"}}}}],
                "discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":{offset}}}]}}"#
        );

        serde_json::from_str(&json).expect("instruction fixture")
    }

    /// Events padding differently behind one tag would each need their own
    /// strip, but the generated parser has a single `EVENT_PAYLOAD_OFFSET`.
    #[test]
    fn program_envelope_rejects_mismatched_payload_offsets() {
        let events = vec![
            event(
                "alpha",
                &format!("{},{}", constant("fe", 0), constant("a1a2", 4)),
            ),
            event(
                "beta",
                &format!("{},{}", constant("fe", 0), constant("b1b2", 6)),
            ),
        ];

        let err = program_envelope(&events).expect_err("mismatched payload offsets must fail");

        assert!(
            err.contains(
                "share a CPI event envelope but start their payloads at different offsets (4 vs 6)"
            ),
            "unexpected message: {err}",
        );
    }

    #[test]
    fn program_envelope_accepts_matching_payload_offsets() {
        let events = vec![
            event(
                "alpha",
                &format!("{},{}", constant("fe", 0), constant("a1a2", 4)),
            ),
            event(
                "beta",
                &format!("{},{}", constant("fe", 0), constant("b1b2", 4)),
            ),
        ];

        let envelope = program_envelope(&events)
            .expect("matching offsets are valid")
            .expect("an envelope was declared");

        assert_eq!(envelope.envelope.discriminator, vec![0xfe]);
        assert_eq!(envelope.envelope.payload_offset, 4);
    }

    /// A program mixing `emit_cpi!` and `emit!` events must not fail.
    #[test]
    fn program_envelope_allows_events_without_an_envelope() {
        let events = vec![
            event(
                "enveloped",
                &format!("{},{}", constant("fe", 0), constant("a1a2", 4)),
            ),
            event("logOnly", &constant("b1b2", 0)),
        ];

        let envelope = program_envelope(&events)
            .expect("a mixed program is valid")
            .expect("the enveloped event still declares one");

        let resolved = envelope.resolve(&events);

        assert!(resolved[0].is_some(), "enveloped event keeps its envelope");
        assert!(resolved[1].is_none(), "log-only event has none");
    }

    /// A chain behind a tag must be all constants; a mixed one cannot be rebased.
    #[test]
    fn envelope_of_rejects_a_mixed_chain_behind_a_tag() {
        let json = format!(
            r#"{{"kind":"eventNode","name":"mixed","data":{{"kind":"structTypeNode","fields":[]}},
                "discriminators":[{},{{"kind":"fieldDiscriminatorNode","name":"d","offset":8}}]}}"#,
            constant("fe", 0),
        );

        let event: EventNode = serde_json::from_str(&json).expect("event fixture");
        let err = envelope_of(&event).expect_err("a mixed chain must fail");

        assert!(err.contains("mixes constant and non-constant"), "{err}");
    }

    /// A single non-constant discriminator is still just an unenveloped event.
    #[test]
    fn envelope_of_allows_a_lone_non_constant_discriminator() {
        let json = r#"{"kind":"eventNode","name":"fieldOnly","data":{"kind":"structTypeNode","fields":[]},
             "discriminators":[{"kind":"fieldDiscriminatorNode","name":"d","offset":0}]}"#;

        let event: EventNode = serde_json::from_str(json).expect("event fixture");

        assert_eq!(envelope_of(&event).expect("not an error"), None);
    }

    /// An event whose only discriminator is the program tag has none of its own.
    #[test]
    fn program_envelope_rejects_an_event_that_is_only_the_tag() {
        let events = vec![
            event(
                "enveloped",
                &format!("{},{}", constant("fe", 0), constant("a1a2", 4)),
            ),
            event("tagOnly", &constant("fe", 0)),
        ];

        let err = program_envelope(&events).expect_err("tag-only event must fail");

        assert!(
            err.contains("declares only the CPI event envelope"),
            "{err}"
        );
    }

    /// A normal single-discriminator event, the shape of every existing IDL,
    /// must stay accepted alongside an enveloped one.
    #[test]
    fn program_envelope_keeps_ordinary_single_discriminator_events() {
        let events = vec![
            event(
                "enveloped",
                &format!("{},{}", constant("fe", 0), constant("a1a2", 4)),
            ),
            event("logOnly", &constant("b1b2", 0)),
        ];

        let envelope = program_envelope(&events)
            .expect("an ordinary event must not be rejected")
            .expect("the enveloped event declares one");

        let resolved = envelope.resolve(&events);

        assert!(resolved[0].is_some());
        assert!(resolved[1].is_none());
    }

    /// A one-byte envelope masks a one-byte instruction discriminator entirely.
    #[test]
    fn collision_detected_when_envelope_masks_an_instruction() {
        let instructions = vec![instruction("swapBaseIn", "09", 0)];

        let message = envelope_instruction_collision(&[0x09], &instructions)
            .expect("a masked instruction must be reported");

        assert!(
            message.contains("envelope 09 collides with instruction `swapBaseIn`"),
            "{message}"
        );
        assert!(message.contains("would never parse"), "{message}");
    }

    /// The envelope is compared only against instructions, never instruction to
    /// instruction, so variants that deliberately share a discriminator are not
    /// the guard's business.
    #[test]
    fn no_collision_when_instructions_share_a_discriminator_but_envelope_differs() {
        let instructions = vec![
            instruction("swapBaseIn", "09", 0),
            instruction("swapBaseInCompact", "09", 0),
        ];

        assert!(envelope_instruction_collision(&[0xfe], &instructions).is_none());
    }

    /// A shorter instruction discriminator that prefixes the tag is a partial
    /// mask: data continuing with the tag's remaining bytes is swallowed.
    #[test]
    fn collision_detected_when_instruction_prefixes_the_envelope() {
        let instructions = vec![instruction("swapBaseIn", "09", 0)];

        assert!(envelope_instruction_collision(&[0x09, 0xaa], &instructions).is_some());
    }

    #[test]
    fn no_collision_for_the_anchor_tag_against_ordinary_instructions() {
        let anchor = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];
        let instructions = vec![instruction("route", "0102030405060708", 0)];

        assert!(envelope_instruction_collision(&anchor, &instructions).is_none());
    }

    fn idl_with_origin(origin: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("shipstern-origin-tests");

        std::fs::create_dir_all(&dir).expect("temp dir");

        // Hex-encode the name: macOS is case-insensitive, so `Anchor.json` and
        // `anchor.json` would be the same file and the cases would clobber each other.
        let stem: String = origin.bytes().map(|b| format!("{b:02x}")).collect();

        let path = dir.join(format!("{stem}.json"));

        std::fs::write(
            &path,
            format!(
                r#"{{"kind":"rootNode","standard":"codama","version":"1.6.0",
                   "program":{{"kind":"programNode","name":"p",
                   "publicKey":"11111111111111111111111111111111","version":"0.1.0",
                   "origin":"{origin}","accounts":[],"instructions":[],
                   "definedTypes":[],"errors":[],"constants":[]}},
                   "additionalPrograms":[]}}"#
            ),
        )
        .expect("write fixture");

        path
    }

    ///
    /// codama owns the origin invariant from 0.13 on, so the macro does not
    /// re-check it.
    ///
    /// `origin` was a free-form `Option<String>` on codama 0.9, which meant a
    /// generator-specific value deserialized and was then silently dropped. It is
    /// a closed enum now, so the same IDL fails to parse and the message names
    /// both the offending value and the accepted set. A wrapper check here would
    /// only restate that, worse.
    ///
    #[test]
    fn codama_rejects_an_unknown_program_origin() {
        let err = load_idl(idl_with_origin("custom")).expect_err("custom is not an origin");

        let message = err.to_string();

        assert!(message.contains("custom"), "{message}");
        assert!(message.contains("anchor"), "{message}");
        assert!(message.contains("shank"), "{message}");
    }

    /// The enum is lowercase-tagged, so a case variant is a different token.
    #[test]
    fn codama_rejects_a_case_variant_origin() {
        assert!(load_idl(idl_with_origin("Anchor")).is_err());
    }

    #[test]
    fn codama_accepts_the_two_real_origins() {
        assert!(load_idl(idl_with_origin("anchor")).is_ok());
        assert!(load_idl(idl_with_origin("shank")).is_ok());
    }
}
