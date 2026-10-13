//! Kafka sink builder that accepts Shipstern parsers as configuration.
//!
//! This module provides a clean API for configuring kafka-sink with Shipstern parsers.
//! Users pass their Shipstern parser implementations, and kafka-sink handles the rest.
//!
//! All parsed outputs are serialized using protobuf (prost::Message::encode).

use std::{
    collections::{BTreeSet, HashMap},
    future::Future,
    pin::Pin,
    sync::Arc,
};

use prost::Message;
use shipstern_core::{
    bs58,
    instruction::{InstructionUpdate, Path},
    AccountUpdate, ParseError, Parser, ProgramParser, Pubkey,
};

use crate::{
    events::{PreparedRecord, RawAccountEvent, RawInstructionEvent, RecordHeader, RecordKind},
    schema_registry::{wrap_payload_with_confluent_wire_format, RegisteredSchema},
    utils::{make_account_record_key, make_instruction_record_key},
};

/// Parsed output result with protobuf-encoded bytes.
#[derive(Debug, Clone)]
pub struct ParsedOutput {
    /// Protobuf-encoded bytes (via prost::Message::encode_to_vec).
    pub data: Vec<u8>,
}

impl ParsedOutput {
    /// Create from any prost::Message type.
    pub fn from_proto<T: Message>(output: &T) -> Self {
        Self {
            data: output.encode_to_vec(),
        }
    }
}

/// Outcome of a single parse attempt. Distinguishes successful parse from
/// filtered (expected, parser doesn't handle this input) and error (unexpected failure).
pub enum ParseOutcome {
    Parsed(ParsedOutput),
    Filtered,
    Error(String),
}

// --- DynInstructionParser ---

/// Type-erased instruction parser trait.
/// This allows storing different parser types in a collection.
pub trait DynInstructionParser: Send + Sync {
    /// Try to parse an instruction.
    fn try_parse<'a>(
        &'a self,
        ix: &'a InstructionUpdate,
    ) -> Pin<Box<dyn Future<Output = ParseOutcome> + Send + 'a>>;

    fn topic(&self) -> &str;

    fn program_name(&self) -> &str;

    fn program_id(&self) -> Pubkey;

    fn fallback_topic(&self) -> Option<&str>;
}

struct InstructionParserWrapper<P> {
    parser: P,
    topic: String,
    program_name: String,
    program_id: Pubkey,
    fallback_topic: Option<String>,
}

impl<P, O> DynInstructionParser for InstructionParserWrapper<P>
where
    P: Parser<Input = InstructionUpdate, Output = O> + ProgramParser + Send + Sync,
    O: Message + Send + Sync,
{
    fn try_parse<'a>(
        &'a self,
        ix: &'a InstructionUpdate,
    ) -> Pin<Box<dyn Future<Output = ParseOutcome> + Send + 'a>> {
        Box::pin(async move {
            match self.parser.parse(ix).await {
                Ok(output) => ParseOutcome::Parsed(ParsedOutput::from_proto(&output)),
                Err(ParseError::Filtered) => ParseOutcome::Filtered,
                Err(e) => {
                    tracing::warn!(?e, program = %self.program_name, "Error parsing instruction");
                    ParseOutcome::Error(e.to_string())
                },
            }
        })
    }

    fn topic(&self) -> &str { &self.topic }

    fn program_name(&self) -> &str { &self.program_name }

    fn program_id(&self) -> Pubkey { self.program_id }

    fn fallback_topic(&self) -> Option<&str> { self.fallback_topic.as_deref() }
}

// --- DynAccountParser ---

/// Type-erased account parser trait.
/// This allows storing different account parser types in a collection.
pub trait DynAccountParser: Send + Sync {
    /// Try to parse an account.
    fn try_parse<'a>(
        &'a self,
        acct: &'a AccountUpdate,
    ) -> Pin<Box<dyn Future<Output = ParseOutcome> + Send + 'a>>;

    fn topic(&self) -> &str;

    fn program_name(&self) -> &str;

    fn fallback_topic(&self) -> Option<&str>;

    fn program_id(&self) -> Pubkey;
}

#[cfg(feature = "experimental-account-parser")]
struct AccountParserWrapper<P> {
    parser: P,
    topic: String,
    program_name: String,
    program_id: Pubkey,
    fallback_topic: Option<String>,
}

#[cfg(feature = "experimental-account-parser")]
impl<P, O> DynAccountParser for AccountParserWrapper<P>
where
    P: Parser<Input = AccountUpdate, Output = O> + ProgramParser + Send + Sync,
    O: Message + Send + Sync,
{
    fn try_parse<'a>(
        &'a self,
        acct: &'a AccountUpdate,
    ) -> Pin<Box<dyn Future<Output = ParseOutcome> + Send + 'a>> {
        Box::pin(async move {
            match self.parser.parse(acct).await {
                Ok(output) => ParseOutcome::Parsed(ParsedOutput::from_proto(&output)),
                Err(ParseError::Filtered) => ParseOutcome::Filtered,
                Err(e) => {
                    tracing::warn!(?e, program = %self.program_name, "Error parsing account");
                    ParseOutcome::Error(e.to_string())
                },
            }
        })
    }

    fn topic(&self) -> &str { &self.topic }

    fn program_name(&self) -> &str { &self.program_name }

    fn fallback_topic(&self) -> Option<&str> { self.fallback_topic.as_deref() }

    fn program_id(&self) -> Pubkey { self.program_id }
}

pub struct KafkaSinkBuilder {
    instruction_parsers: Vec<Arc<dyn DynInstructionParser>>,
    account_parsers: Vec<Arc<dyn DynAccountParser>>,
}

fn collect_topics<'a>(
    instruction_parsers: &'a [Arc<dyn DynInstructionParser>],
    account_parsers: &'a [Arc<dyn DynAccountParser>],
) -> Vec<&'a str> {
    instruction_parsers
        .iter()
        .flat_map(|p| std::iter::once(p.topic()).chain(p.fallback_topic()))
        .chain(
            account_parsers
                .iter()
                .flat_map(|p| std::iter::once(p.topic()).chain(p.fallback_topic())),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl Default for KafkaSinkBuilder {
    fn default() -> Self { Self::new() }
}

impl KafkaSinkBuilder {
    pub fn new() -> Self {
        Self {
            instruction_parsers: Vec::new(),
            account_parsers: Vec::new(),
        }
    }

    /// Add an instruction parser with its program name and Kafka topic.
    pub fn instruction_parser<P, O>(mut self, parser: P, program_name: &str, topic: &str) -> Self
    where
        P: Parser<Input = InstructionUpdate, Output = O> + ProgramParser + Send + Sync + 'static,
        O: Message + Send + Sync + 'static,
    {
        let program_id = parser.program_id();

        self.instruction_parsers
            .push(Arc::new(InstructionParserWrapper {
                parser,
                topic: topic.to_string(),
                program_name: program_name.to_string(),
                program_id,
                fallback_topic: None,
            }));

        self
    }

    /// Add an account parser with its program name and Kafka topic.
    #[cfg(feature = "experimental-account-parser")]
    pub fn account_parser<P, O>(mut self, parser: P, program_name: &str, topic: &str) -> Self
    where
        P: Parser<Input = AccountUpdate, Output = O> + ProgramParser + Send + Sync + 'static,
        O: Message + Send + Sync + 'static,
    {
        let program_id = parser.program_id();

        self.account_parsers.push(Arc::new(AccountParserWrapper {
            parser,
            topic: topic.to_string(),
            program_name: program_name.to_string(),
            program_id,
            fallback_topic: None,
        }));

        self
    }

    /// Add an instruction parser with a per-parser fallback topic.
    pub fn instruction_parser_with_fallback<P, O>(
        mut self,
        parser: P,
        program_name: &str,
        topic: &str,
        fallback_topic: &str,
    ) -> Self
    where
        P: Parser<Input = InstructionUpdate, Output = O> + ProgramParser + Send + Sync + 'static,
        O: Message + Send + Sync + 'static,
    {
        let program_id = parser.program_id();

        self.instruction_parsers
            .push(Arc::new(InstructionParserWrapper {
                parser,
                topic: topic.to_string(),
                program_name: program_name.to_string(),
                program_id,
                fallback_topic: Some(fallback_topic.to_string()),
            }));

        self
    }

    /// Add an account parser with a per-parser fallback topic.
    #[cfg(feature = "experimental-account-parser")]
    pub fn account_parser_with_fallback<P, O>(
        mut self,
        parser: P,
        program_name: &str,
        topic: &str,
        fallback_topic: &str,
    ) -> Self
    where
        P: Parser<Input = AccountUpdate, Output = O> + ProgramParser + Send + Sync + 'static,
        O: Message + Send + Sync + 'static,
    {
        let program_id = parser.program_id();

        self.account_parsers.push(Arc::new(AccountParserWrapper {
            parser,
            topic: topic.to_string(),
            program_name: program_name.to_string(),
            program_id,
            fallback_topic: Some(fallback_topic.to_string()),
        }));

        self
    }

    pub fn build(self) -> KafkaSink {
        KafkaSink {
            instruction_parsers: self.instruction_parsers,
            account_parsers: self.account_parsers,
            schema_ids: HashMap::new(),
        }
    }

    pub fn topics(&self) -> Vec<&str> {
        collect_topics(&self.instruction_parsers, &self.account_parsers)
    }
}

// --- KafkaSink (formerly ConfiguredParsers) ---

#[derive(Clone, Default)]
pub struct KafkaSink {
    instruction_parsers: Vec<Arc<dyn DynInstructionParser>>,
    account_parsers: Vec<Arc<dyn DynAccountParser>>,
    /// Map of topic -> schema info for encoding with Confluent wire format.
    schema_ids: HashMap<String, RegisteredSchema>,
}

struct InstructionRecordContext<'a> {
    slot: u64,
    tx_index: u64,
    signature: &'a [u8],
    fee_payer: Option<&'a [u8]>,
    path: &'a Path,
}

impl KafkaSink {
    pub fn topics(&self) -> Vec<&str> {
        collect_topics(&self.instruction_parsers, &self.account_parsers)
    }

    /// Returns true if any transaction-derived work is configured.
    pub fn has_transaction_work(&self) -> bool { !self.instruction_parsers.is_empty() }

    /// Returns true if any account parsers are registered.
    pub fn has_account_parsers(&self) -> bool { !self.account_parsers.is_empty() }

    /// Returns the account parsers (used by AccountSubscription to build prefilter).
    pub fn account_parsers(&self) -> &[Arc<dyn DynAccountParser>] { &self.account_parsers }

    /// Set schema IDs for encoding messages with Confluent wire format.
    /// The key should be the subject name (e.g., "spl-token.instructions-value").
    pub fn set_schema_ids(&mut self, schemas: HashMap<String, RegisteredSchema>) {
        for (subject, schema) in &schemas {
            tracing::debug!(
                subject,
                schema_id = schema.schema_id,
                message_index = schema.message_index,
                "Registered schema for encoding"
            );
        }

        self.schema_ids = schemas;
    }

    /// Get schema ID for a topic (looks up "<topic>-value" subject).
    fn get_schema_for_topic(&self, topic: &str) -> Option<&RegisteredSchema> {
        let subject = format!("{}-value", topic);

        let result = self.schema_ids.get(&subject);

        if result.is_none() {
            tracing::warn!(
                topic,
                subject,
                available_subjects = ?self.schema_ids.keys().collect::<Vec<_>>(),
                "No schema found for topic"
            );
        }

        result
    }

    // --- Instruction parsing ---

    /// Parse an instruction with the first matching parser and prepare a Kafka
    /// record, plus `had_error` for an unexpected failure (not a filter). Parse
    /// errors may go to the configured fallback.
    ///
    /// `path` is the flat index used in Kafka keys and `ix_index` headers, from
    /// [`InstructionUpdate::visit_all_with_flat_indices`], not the nested `ix.path`.
    pub async fn parse_instruction(
        &self,
        slot: u64,
        tx_index: u64,
        signature: &[u8],
        path: &Path,
        ix: &InstructionUpdate,
    ) -> (Option<PreparedRecord>, bool) {
        let mut had_error = false;
        let context = InstructionRecordContext {
            slot,
            tx_index,
            signature,
            fee_payer: Self::instruction_fee_payer(ix),
            path,
        };

        for parser in &self.instruction_parsers {
            // Only dispatch to parsers for this instruction's program.
            if ix.program != parser.program_id() {
                continue;
            }

            match parser.try_parse(ix).await {
                ParseOutcome::Parsed(parsed) => {
                    let record =
                        self.prepare_decoded_instruction_record(&context, parsed, parser.topic());

                    return (Some(record), false);
                },
                ParseOutcome::Filtered => {
                    // Filtered means "not decoded" but not an error, so no fallback emission.
                },
                ParseOutcome::Error(e) => {
                    had_error = true;

                    if let Some(fallback) = parser.fallback_topic() {
                        let record =
                            self.prepare_fallback_instruction_record(&context, ix, fallback, &e);

                        return (Some(record), true);
                    }
                },
            }
        }
        (None, had_error)
    }

    /// Build the base headers and key shared by all instruction record types.
    fn instruction_base_record(
        context: &InstructionRecordContext<'_>,
    ) -> (String, Vec<RecordHeader>) {
        let sig_str = bs58::encode(context.signature).into_string();

        let path_str = format!("{:?}", context.path);

        let key = make_instruction_record_key(context.slot, &sig_str, &path_str);

        let headers = vec![
            RecordHeader {
                key: "slot",
                value: context.slot.to_string(),
            },
            RecordHeader {
                key: "signature",
                value: sig_str,
            },
            RecordHeader {
                key: "tx_index",
                value: context.tx_index.to_string(),
            },
            RecordHeader {
                key: "ix_index",
                value: path_str,
            },
        ];
        let headers = if let Some(fee_payer) = context.fee_payer {
            let mut headers = headers;
            headers.push(RecordHeader {
                key: "fee_payer",
                value: bs58::encode(fee_payer).into_string(),
            });
            headers
        } else {
            headers
        };

        (key, headers)
    }

    /// Encode payload with Confluent wire format if a schema is registered for the topic,
    /// otherwise return the raw protobuf bytes.
    fn encode_payload_for_topic(&self, topic: &str, raw_data: Vec<u8>) -> Vec<u8> {
        match self.get_schema_for_topic(topic) {
            Some(schema) => {
                let indices: &[i32] = if schema.message_index == 0 {
                    &[] // First message: empty array per Confluent spec
                } else {
                    &[schema.message_index]
                };

                wrap_payload_with_confluent_wire_format(schema.schema_id, indices, &raw_data)
            },
            None => raw_data,
        }
    }

    /// Prepare a record for a successfully decoded instruction.
    /// Payload is protobuf-encoded with Confluent wire format, metadata goes in Kafka headers.
    fn prepare_decoded_instruction_record(
        &self,
        context: &InstructionRecordContext<'_>,
        parsed: ParsedOutput,
        topic: &str,
    ) -> PreparedRecord {
        let (key, headers) = Self::instruction_base_record(context);

        let payload = self.encode_payload_for_topic(topic, parsed.data);

        PreparedRecord {
            topic: topic.to_string(),
            payload,
            key,
            headers,
            is_decoded: true,
            kind: RecordKind::Instruction,
        }
    }

    /// Prepare a fallback record for unrecognized instructions.
    /// Payload is plain JSON (`RawInstructionEvent`), metadata in headers.
    fn prepare_fallback_instruction_record(
        &self,
        context: &InstructionRecordContext<'_>,
        ix: &InstructionUpdate,
        fallback_topic: &str,
        error: &str,
    ) -> PreparedRecord {
        let (key, mut headers) = Self::instruction_base_record(context);

        let program_id = bs58::encode(ix.program).into_string();

        headers.push(RecordHeader {
            key: "program_id",
            value: program_id.clone(),
        });

        headers.push(RecordHeader {
            key: "parse_error",
            value: error.to_string(),
        });

        let fallback_event = RawInstructionEvent {
            slot: context.slot,
            signature: bs58::encode(context.signature).into_string(),
            ix_index: format!("{:?}", context.path),
            program_id: program_id.clone(),
            data: bs58::encode(&ix.data).into_string(),
        };

        let payload = serde_json::to_vec(&fallback_event)
            .expect("RawInstructionEvent serialization is infallible");

        PreparedRecord {
            topic: fallback_topic.to_string(),
            payload,
            key,
            headers,
            is_decoded: false,
            kind: RecordKind::Instruction,
        }
    }

    fn instruction_fee_payer(ix: &InstructionUpdate) -> Option<&[u8]> {
        ix.shared.accounts.static_keys.first().map(Vec::as_slice)
    }

    // --- Subscription constructors ---

    /// Create a TransactionSubscription for this sink, if transaction-derived work exists.
    pub fn transaction_subscription(&self) -> Option<crate::parsers::TransactionSubscription> {
        if self.has_transaction_work() {
            Some(crate::parsers::TransactionSubscription)
        } else {
            None
        }
    }

    /// Create an AccountSubscription for this sink, if any account parsers are registered.
    /// Returns `None` if no account parsers were configured.
    pub fn account_subscription(&self) -> Option<crate::parsers::AccountSubscription> {
        crate::parsers::AccountSubscription::new(self)
    }

    // --- Account parsing ---

    /// Parse an account update and prepare a Kafka record.
    ///
    /// Tries each registered account parser associated with the account owner. On match, builds a
    /// decoded account record.
    /// On `Error`, checks if that parser has a fallback_topic.
    /// Returns the record (if any) and a `had_error` flag.
    pub async fn parse_account(
        &self,
        slot: u64,
        acct: &AccountUpdate,
    ) -> (Option<PreparedRecord>, bool) {
        let inner = match acct.account.as_ref() {
            Some(inner) => inner,
            None => return (None, false),
        };

        let pubkey_str = bs58::encode(&inner.pubkey).into_string();
        let owner_str = bs58::encode(&inner.owner).into_string();
        let write_version = inner.write_version;

        let mut had_error = false;

        for parser in &self.account_parsers {
            if parser.program_id().0.as_slice() != inner.owner.as_slice() {
                continue;
            }

            match parser.try_parse(acct).await {
                ParseOutcome::Parsed(parsed) => {
                    return (
                        Some(self.prepare_decoded_account_record(
                            slot,
                            &pubkey_str,
                            write_version,
                            &owner_str,
                            parsed,
                            parser.topic(),
                        )),
                        false,
                    );
                },
                ParseOutcome::Filtered => {
                    // Filtered means "not decoded" but not an error, so no fallback emission.
                },
                ParseOutcome::Error(e) => {
                    had_error = true;

                    if let Some(fallback) = parser.fallback_topic() {
                        return (
                            Some(self.prepare_fallback_account_record(
                                slot,
                                &pubkey_str,
                                write_version,
                                &owner_str,
                                &inner.data,
                                fallback,
                                &e,
                            )),
                            true,
                        );
                    }
                },
            }
        }
        (None, had_error)
    }

    /// Prepare a record for a successfully decoded account.
    fn prepare_decoded_account_record(
        &self,
        slot: u64,
        pubkey: &str,
        write_version: u64,
        owner: &str,
        parsed: ParsedOutput,
        topic: &str,
    ) -> PreparedRecord {
        let key = make_account_record_key(slot, pubkey, write_version);
        let payload = self.encode_payload_for_topic(topic, parsed.data);

        let headers = vec![
            RecordHeader {
                key: "slot",
                value: slot.to_string(),
            },
            RecordHeader {
                key: "pubkey",
                value: pubkey.to_string(),
            },
            RecordHeader {
                key: "write_version",
                value: write_version.to_string(),
            },
            RecordHeader {
                key: "owner",
                value: owner.to_string(),
            },
        ];

        PreparedRecord {
            topic: topic.to_string(),
            payload,
            key,
            headers,
            is_decoded: true,
            kind: RecordKind::Account,
        }
    }

    /// Prepare a fallback record for accounts that failed to parse.
    /// Payload is plain JSON (`RawAccountEvent`), metadata in headers.
    #[allow(clippy::too_many_arguments)]
    fn prepare_fallback_account_record(
        &self,
        slot: u64,
        pubkey: &str,
        write_version: u64,
        owner: &str,
        data: &[u8],
        fallback_topic: &str,
        error: &str,
    ) -> PreparedRecord {
        let key = make_account_record_key(slot, pubkey, write_version);

        let headers = vec![
            RecordHeader {
                key: "slot",
                value: slot.to_string(),
            },
            RecordHeader {
                key: "pubkey",
                value: pubkey.to_string(),
            },
            RecordHeader {
                key: "write_version",
                value: write_version.to_string(),
            },
            RecordHeader {
                key: "owner",
                value: owner.to_string(),
            },
            RecordHeader {
                key: "parse_error",
                value: error.to_string(),
            },
        ];

        let fallback_event = RawAccountEvent {
            slot,
            pubkey: pubkey.to_string(),
            write_version,
            owner: owner.to_string(),
            data: bs58::encode(data).into_string(),
        };

        let payload = serde_json::to_vec(&fallback_event)
            .expect("RawAccountEvent serialization is infallible");

        PreparedRecord {
            topic: fallback_topic.to_string(),
            payload,
            key,
            headers,
            is_decoded: false,
            kind: RecordKind::Account,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use prost::Message;
    use shipstern_core::{
        instruction::{AccountKeys, InstructionShared, InstructionUpdate, Path},
        ParseError, ParseResult, Parser, Prefilter, ProgramParser, Pubkey,
    };
    #[cfg(feature = "experimental-account-parser")]
    use shipstern_core::{AccountUpdate, AccountUpdateInfo};

    use super::KafkaSinkBuilder;
    #[cfg(feature = "experimental-account-parser")]
    use crate::events::RawAccountEvent;
    use crate::events::RawInstructionEvent;

    #[derive(Clone, Copy)]
    enum TestInstructionOutcome {
        Parsed,
        Filtered,
        Error,
    }

    #[derive(Clone)]
    struct TestInstructionParser {
        program_id: Pubkey,
        outcome: TestInstructionOutcome,
        calls: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct PathRecordingInstructionParser {
        program_id: Pubkey,
        seen_path: Arc<std::sync::Mutex<Option<String>>>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct TestInstructionMessage {
        #[prost(uint64, tag = "1")]
        value: u64,
    }

    impl Parser for TestInstructionParser {
        type Input = InstructionUpdate;
        type Output = TestInstructionMessage;

        fn id(&self) -> Cow<'static, str> { "test-instruction-parser".into() }

        fn prefilter(&self) -> Prefilter { Prefilter::default() }

        async fn parse(&self, _value: &Self::Input) -> ParseResult<Self::Output> {
            self.calls.fetch_add(1, Ordering::Relaxed);

            match self.outcome {
                TestInstructionOutcome::Parsed => Ok(TestInstructionMessage { value: 42 }),
                TestInstructionOutcome::Filtered => Err(ParseError::Filtered),
                TestInstructionOutcome::Error => {
                    Err(ParseError::from(std::io::Error::other("test parser error")))
                },
            }
        }
    }

    impl ProgramParser for TestInstructionParser {
        fn program_id(&self) -> Pubkey { self.program_id }
    }

    impl Parser for PathRecordingInstructionParser {
        type Input = InstructionUpdate;
        type Output = TestInstructionMessage;

        fn id(&self) -> Cow<'static, str> { "path-recording-instruction-parser".into() }

        fn prefilter(&self) -> Prefilter { Prefilter::default() }

        async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> {
            *self.seen_path.lock().expect("path recorder mutex poisoned") =
                Some(format!("{:?}", value.path));

            Ok(TestInstructionMessage { value: 42 })
        }
    }

    impl ProgramParser for PathRecordingInstructionParser {
        fn program_id(&self) -> Pubkey { self.program_id }
    }

    #[cfg(feature = "experimental-account-parser")]
    #[derive(Clone, Copy)]
    enum TestAccountOutcome {
        Parsed,
        Filtered,
        Error,
    }

    #[cfg(feature = "experimental-account-parser")]
    #[derive(Clone)]
    struct TestAccountParser {
        program_id: Pubkey,
        outcome: TestAccountOutcome,
        calls: Arc<AtomicUsize>,
    }

    #[cfg(feature = "experimental-account-parser")]
    #[derive(Clone, PartialEq, Message)]
    struct TestAccountMessage {
        #[prost(uint64, tag = "1")]
        value: u64,
    }

    #[cfg(feature = "experimental-account-parser")]
    impl Parser for TestAccountParser {
        type Input = AccountUpdate;
        type Output = TestAccountMessage;

        fn id(&self) -> Cow<'static, str> { "test-account-parser".into() }

        fn prefilter(&self) -> Prefilter { Prefilter::default() }

        async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> {
            self.calls.fetch_add(1, Ordering::Relaxed);

            let owner = value
                .account
                .as_ref()
                .map(|a| a.owner.as_slice())
                .unwrap_or_default();

            if owner != self.program_id.0 {
                return Err(ParseError::Filtered);
            }

            match self.outcome {
                TestAccountOutcome::Parsed => Ok(TestAccountMessage { value: 7 }),
                TestAccountOutcome::Filtered => Err(ParseError::Filtered),
                TestAccountOutcome::Error => Err(ParseError::from(std::io::Error::other(
                    "test account parser error",
                ))),
            }
        }
    }

    #[cfg(feature = "experimental-account-parser")]
    impl ProgramParser for TestAccountParser {
        fn program_id(&self) -> Pubkey { self.program_id }
    }

    fn instruction_with_program(program: Pubkey) -> InstructionUpdate {
        InstructionUpdate {
            program,
            accounts: vec![],
            data: vec![1, 2, 3],
            shared: Arc::new(InstructionShared {
                accounts: AccountKeys {
                    static_keys: vec![vec![4_u8; 32]],
                    dynamic_rw: vec![],
                    dynamic_ro: vec![],
                },
                ..InstructionShared::default()
            }),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        }
    }

    #[cfg(feature = "experimental-account-parser")]
    fn account_with_owner(owner: Pubkey) -> AccountUpdate {
        AccountUpdate {
            slot: 100,
            bank_id: Some(0),
            is_startup: false,
            account: Some(AccountUpdateInfo {
                txn_signature: None,
                write_version: 11,
                pubkey: vec![2_u8; 32],
                data: vec![9_u8, 8, 7],
                executable: false,
                lamports: 1,
                owner: owner.0.to_vec(),
                rent_epoch: 0,
            }),
        }
    }

    #[test]
    fn unrelated_instruction_does_not_route_to_fallback_topic() {
        let calls = Arc::new(AtomicUsize::new(0));

        let parser = TestInstructionParser {
            program_id: [1; 32].into(),
            outcome: TestInstructionOutcome::Filtered,
            calls: Arc::clone(&calls),
        };

        let sink = KafkaSinkBuilder::new()
            .instruction_parser_with_fallback(
                parser,
                "test",
                "test.instructions",
                "failed.test.instructions",
            )
            .build();

        let ix = instruction_with_program([9; 32].into());

        let (record, had_error) =
            futures::executor::block_on(sink.parse_instruction(100, 7, b"sig", &ix.path, &ix));

        assert!(
            record.is_none(),
            "unexpected fallback for unrelated instruction"
        );
        assert!(!had_error);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn related_filtered_instruction_does_not_route_to_fallback_topic() {
        let parser = TestInstructionParser {
            program_id: [1; 32].into(),
            outcome: TestInstructionOutcome::Filtered,
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let sink = KafkaSinkBuilder::new()
            .instruction_parser_with_fallback(
                parser,
                "test",
                "test.instructions",
                "failed.test.instructions",
            )
            .build();

        let ix = instruction_with_program([1; 32].into());

        let (record, had_error) =
            futures::executor::block_on(sink.parse_instruction(100, 7, b"sig", &ix.path, &ix));

        assert!(record.is_none(), "filtered decode should not emit fallback");
        assert!(!had_error);
    }

    #[test]
    fn related_parse_error_routes_to_fallback_and_marks_error() {
        let parser = TestInstructionParser {
            program_id: [1; 32].into(),
            outcome: TestInstructionOutcome::Error,
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let sink = KafkaSinkBuilder::new()
            .instruction_parser_with_fallback(
                parser,
                "test",
                "test.instructions",
                "failed.test.instructions",
            )
            .build();

        let ix = instruction_with_program([1; 32].into());

        let (record, had_error) =
            futures::executor::block_on(sink.parse_instruction(100, 7, b"sig", &ix.path, &ix));

        let record = record.expect("expected fallback record");

        assert_eq!(record.topic, "failed.test.instructions");
        assert!(had_error);

        let event: RawInstructionEvent =
            serde_json::from_slice(&record.payload).expect("fallback payload must be JSON");

        assert_eq!(event.slot, 100);
        assert_eq!(
            event.signature,
            shipstern_core::bs58::encode(b"sig").into_string()
        );
        assert_eq!(event.ix_index, "1");
        assert_eq!(
            event.program_id,
            shipstern_core::bs58::encode([1_u8; 32]).into_string()
        );
        assert_eq!(
            event.data,
            shipstern_core::bs58::encode([1_u8, 2, 3]).into_string()
        );
        assert_eq!(
            record
                .headers
                .iter()
                .find(|header| header.key == "tx_index")
                .map(|header| header.value.as_str()),
            Some("7")
        );
        let expected_fee_payer = shipstern_core::bs58::encode([4_u8; 32]).into_string();
        assert_eq!(
            record
                .headers
                .iter()
                .find(|header| header.key == "fee_payer")
                .map(|header| header.value.as_str()),
            Some(expected_fee_payer.as_str())
        );
    }

    #[test]
    fn related_parsed_instruction_uses_primary_topic() {
        let parser = TestInstructionParser {
            program_id: [1; 32].into(),
            outcome: TestInstructionOutcome::Parsed,
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let sink = KafkaSinkBuilder::new()
            .instruction_parser_with_fallback(
                parser,
                "test",
                "test.instructions",
                "failed.test.instructions",
            )
            .build();

        let ix = instruction_with_program([1; 32].into());

        let (record, had_error) =
            futures::executor::block_on(sink.parse_instruction(100, 7, b"sig", &ix.path, &ix));

        let record = record.expect("expected decoded record");

        assert_eq!(record.topic, "test.instructions");
        assert!(!had_error);
        assert!(record.is_decoded);
        assert_eq!(
            record
                .headers
                .iter()
                .find(|header| header.key == "tx_index")
                .map(|header| header.value.as_str()),
            Some("7")
        );
        let expected_fee_payer = shipstern_core::bs58::encode([4_u8; 32]).into_string();
        assert_eq!(
            record
                .headers
                .iter()
                .find(|header| header.key == "fee_payer")
                .map(|header| header.value.as_str()),
            Some(expected_fee_payer.as_str())
        );
    }

    #[test]
    fn instruction_record_ix_index_uses_flat_index_without_rewriting_parser_path() {
        let seen_path = Arc::new(std::sync::Mutex::new(None));
        let parser = PathRecordingInstructionParser {
            program_id: [1; 32].into(),
            seen_path: Arc::clone(&seen_path),
        };

        let sink = KafkaSinkBuilder::new()
            .instruction_parser(parser, "test", "test.instructions")
            .build();

        let mut ix = instruction_with_program([1; 32].into());
        ix.path = Path::from(vec![2, 2]);
        let flat_index = Path::from(vec![2, 6]);

        let (record, had_error) =
            futures::executor::block_on(sink.parse_instruction(100, 7, b"sig", &flat_index, &ix));

        let record = record.expect("expected decoded record");

        assert!(!had_error);
        // Kafka ix_index is the flat index even though the parser still sees
        // the richer nested call-tree path.
        assert_eq!(
            record
                .headers
                .iter()
                .find(|header| header.key == "ix_index")
                .map(|header| header.value.as_str()),
            Some("3.7")
        );
        let sig_str = shipstern_core::bs58::encode(b"sig").into_string();
        assert_eq!(record.key, format!("100:{sig_str}:3.7"));
        assert_eq!(
            *seen_path.lock().expect("path recorder mutex poisoned"),
            Some("3.3".to_owned())
        );
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn related_filtered_account_does_not_route_to_fallback_topic() {
        let parser = TestAccountParser {
            program_id: [1; 32].into(),
            outcome: TestAccountOutcome::Filtered,
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let sink = KafkaSinkBuilder::new()
            .account_parser_with_fallback(parser, "test", "test.accounts", "failed.test.accounts")
            .build();

        let acct = account_with_owner([1; 32].into());
        let (record, had_error) = futures::executor::block_on(sink.parse_account(100, &acct));

        assert!(record.is_none(), "filtered decode should not emit fallback");
        assert!(!had_error);
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn related_parse_error_account_routes_to_fallback_and_marks_error() {
        let parser = TestAccountParser {
            program_id: [1; 32].into(),
            outcome: TestAccountOutcome::Error,
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let sink = KafkaSinkBuilder::new()
            .account_parser_with_fallback(parser, "test", "test.accounts", "failed.test.accounts")
            .build();

        let acct = account_with_owner([1; 32].into());

        let (record, had_error) = futures::executor::block_on(sink.parse_account(100, &acct));

        let record = record.expect("expected fallback record");
        let pubkey = shipstern_core::bs58::encode([2_u8; 32]).into_string();

        assert_eq!(record.topic, "failed.test.accounts");
        assert_eq!(record.key, format!("100:{pubkey}:11"));
        assert!(had_error);

        let event: RawAccountEvent =
            serde_json::from_slice(&record.payload).expect("fallback payload must be JSON");

        assert_eq!(event.slot, 100);
        assert_eq!(event.write_version, 11);
        assert_eq!(
            event.owner,
            shipstern_core::bs58::encode([1_u8; 32]).into_string()
        );
        assert_eq!(
            event.data,
            shipstern_core::bs58::encode([9_u8, 8, 7]).into_string()
        );
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn related_parsed_account_uses_primary_topic() {
        let parser = TestAccountParser {
            program_id: [1; 32].into(),
            outcome: TestAccountOutcome::Parsed,
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let sink = KafkaSinkBuilder::new()
            .account_parser_with_fallback(parser, "test", "test.accounts", "failed.test.accounts")
            .build();

        let acct = account_with_owner([1; 32].into());

        let (record, had_error) = futures::executor::block_on(sink.parse_account(100, &acct));

        let record = record.expect("expected decoded record");
        let pubkey = shipstern_core::bs58::encode([2_u8; 32]).into_string();

        assert_eq!(record.topic, "test.accounts");
        assert_eq!(record.key, format!("100:{pubkey}:11"));
        assert!(!had_error);
        assert!(record.is_decoded);
        assert!(record
            .headers
            .iter()
            .any(|header| { header.key == "write_version" && header.value == "11" }));
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn unrelated_account_does_not_invoke_parser_or_route_to_fallback_topic() {
        let calls = Arc::new(AtomicUsize::new(0));
        let parser = TestAccountParser {
            program_id: [1; 32].into(),
            outcome: TestAccountOutcome::Error,
            calls: Arc::clone(&calls),
        };

        let sink = KafkaSinkBuilder::new()
            .account_parser_with_fallback(parser, "test", "test.accounts", "failed.test.accounts")
            .build();

        let acct = account_with_owner([9; 32].into());
        let (record, had_error) = futures::executor::block_on(sink.parse_account(100, &acct));

        assert!(record.is_none(), "unrelated account must not emit a record");
        assert!(
            !had_error,
            "unrelated account must not count as a parse error"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
