//! Runs transactions through runtimes and reads the counters back from their
//! prometheus registry. The counters are process-wide, so this is the only test
//! in its binary, and each scenario asserts the change it made.

use std::borrow::Cow;

use async_trait::async_trait;
use prometheus::Registry;
use shipstern::{
    config::BufferConfig,
    filter_pipeline::FilterPipeline,
    instruction::InstructionPipeline,
    shipstern_core::{
        instruction::InstructionUpdate, ParseError, ParseResult, Parser, Prefilter,
        TransactionUpdate,
    },
    sources::{SourceContext, SourceExitStatus, SourceTrait},
    Error, Handler, HandlerResult, Pipeline, Runtime,
};
use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateTransactionInfo},
    solana::storage::confirmed_block::{
        CompiledInstruction, Message, MessageHeader, Transaction, TransactionStatusMeta,
    },
};

const TX_PARSER: &str = "test::TxParser";
const INSTRUCTION_PARSER: &str = "test::InstructionParser";
const FILTERING_PARSER: &str = "test::FilteringParser";
const FAILING_PARSER: &str = "test::FailingParser";
const FILTERING_TX_PARSER: &str = "test::FilteringTxParser";

#[derive(Debug, Clone, Copy)]
struct TxParser;

impl Parser for TxParser {
    type Input = TransactionUpdate;
    type Output = ();

    fn id(&self) -> Cow<'static, str> { TX_PARSER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, _: &Self::Input) -> ParseResult<Self::Output> { Ok(()) }
}

/// Parses every instruction.
#[derive(Debug, Clone, Copy)]
struct InstructionParser;

impl Parser for InstructionParser {
    type Input = InstructionUpdate;
    type Output = ();

    fn id(&self) -> Cow<'static, str> { INSTRUCTION_PARSER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, _: &Self::Input) -> ParseResult<Self::Output> { Ok(()) }
}

/// Handles instructions tagged 1, filters out those tagged 2, and fails to
/// parse those tagged 3.
#[derive(Debug, Clone, Copy)]
struct FilteringParser;

impl Parser for FilteringParser {
    type Input = InstructionUpdate;
    type Output = ();

    fn id(&self) -> Cow<'static, str> { FILTERING_PARSER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> {
        match value.data.first() {
            Some(2) => Err(ParseError::Filtered),
            Some(3) => Err(ParseError::Other("simulated unpack failure".into())),
            _ => Ok(()),
        }
    }
}

/// Parses every instruction, and its handler fails on those tagged 2.
#[derive(Debug, Clone, Copy)]
struct FailingParser;

impl Parser for FailingParser {
    type Input = InstructionUpdate;
    type Output = ();

    fn id(&self) -> Cow<'static, str> { FAILING_PARSER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, _: &Self::Input) -> ParseResult<Self::Output> { Ok(()) }
}

/// Filters out transactions of slot 2.
#[derive(Debug, Clone, Copy)]
struct FilteringTxParser;

impl Parser for FilteringTxParser {
    type Input = TransactionUpdate;
    type Output = ();

    fn id(&self) -> Cow<'static, str> { FILTERING_TX_PARSER.into() }

    fn prefilter(&self) -> Prefilter { Prefilter::default() }

    async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> {
        if value.slot == 2 {
            return Err(ParseError::Filtered);
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct NoopHandler;

impl Handler<(), TransactionUpdate> for NoopHandler {
    async fn handle(&self, (): &(), _: &TransactionUpdate) -> HandlerResult<()> { Ok(()) }
}

impl Handler<(), InstructionUpdate> for NoopHandler {
    async fn handle(&self, (): &(), _: &InstructionUpdate) -> HandlerResult<()> { Ok(()) }
}

#[derive(Debug, Clone, Copy)]
struct FailOnTagTwo;

impl Handler<(), InstructionUpdate> for FailOnTagTwo {
    async fn handle(&self, (): &(), raw: &InstructionUpdate) -> HandlerResult<()> {
        if raw.data.first() == Some(&2) {
            return Err("simulated handler failure".into());
        }

        Ok(())
    }
}

fn transaction(tags: &[u8]) -> TransactionUpdate {
    let instructions = tags
        .iter()
        .map(|tag| CompiledInstruction {
            program_id_index: 0,
            accounts: vec![1, 2],
            data: vec![*tag],
        })
        .collect();

    TransactionUpdate {
        slot: 1,
        bank_id: 0,
        transaction: Some(SubscribeUpdateTransactionInfo {
            signature: vec![1; 64],
            is_vote: false,
            index: 0,
            transaction: Some(Transaction {
                signatures: vec![vec![1; 64]],
                message: Some(Message {
                    header: Some(MessageHeader {
                        num_required_signatures: 1,
                        num_readonly_signed_accounts: 0,
                        num_readonly_unsigned_accounts: 0,
                    }),
                    account_keys: vec![[7u8; 32].to_vec(), [8u8; 32].to_vec(), [9u8; 32].to_vec()],
                    recent_blockhash: vec![2; 32],
                    instructions,
                    versioned: false,
                    address_table_lookups: vec![],
                    config: None,
                }),
            }),
            meta: Some(TransactionStatusMeta {
                err: None,
                fee: 0,
                pre_balances: vec![0; 3],
                post_balances: vec![0; 3],
                inner_instructions: vec![],
                inner_instructions_none: false,
                log_messages: vec![],
                log_messages_none: false,
                pre_token_balances: vec![],
                post_token_balances: vec![],
                rewards: vec![],
                loaded_writable_addresses: vec![],
                loaded_readonly_addresses: vec![],
                return_data: None,
                return_data_none: true,
                compute_units_consumed: None,
                cost_units: None,
            }),
        }),
    }
}

#[derive(Debug)]
struct TransactionSource {
    transaction_parser: &'static str,
    transactions: Vec<TransactionUpdate>,
}

#[async_trait]
impl SourceTrait for TransactionSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, Error> {
        let SourceContext { tx, .. } = ctx;

        for transaction in self.transactions.iter().cloned() {
            let update = SubscribeUpdate {
                filters: vec![
                    self.transaction_parser.to_owned(),
                    InstructionPipeline::ID.to_owned(),
                ],
                update_oneof: Some(UpdateOneof::Transaction(transaction)),
                created_at: None,
            };

            let _ = tx.send(Ok(update)).await;
        }

        Ok(SourceExitStatus::Completed)
    }
}

fn counter(registry: &Registry, name: &str) -> u64 {
    let families = registry.gather();
    let family = families
        .iter()
        .find(|family| family.name() == name)
        .unwrap_or_else(|| panic!("{name} is not registered"));

    family.metric[0].counter.value() as u64
}

#[tokio::test]
async fn metrics_count_each_update_once_and_skip_filtered_ones() {
    let registry = Registry::new();

    // Two transactions whose instructions all succeed, and one whose
    // instruction tree cannot be built.
    let mut malformed = transaction(&[1]);

    malformed.transaction.as_mut().unwrap().meta = None;

    Runtime::builder()
        .metrics(registry.clone())
        .transaction(Pipeline::new(TxParser, [NoopHandler]))
        .instruction(Pipeline::new(InstructionParser, [NoopHandler]))
        .try_build_with(
            TransactionSource {
                transaction_parser: TX_PARSER,
                transactions: vec![transaction(&[1, 2]), transaction(&[1]), malformed],
            },
            BufferConfig::default(),
        )
        .unwrap()
        .try_run_async()
        .await
        .unwrap();

    // Each transaction is handled once, by its transaction pipeline. Its
    // instruction pipeline must not add a second success, and failing to build
    // its instruction tree is an instruction parsing error.
    assert_eq!(counter(&registry, "shipstern_transactions_received"), 3);
    assert_eq!(counter(&registry, "shipstern_transactions_successful"), 3);
    assert_eq!(
        counter(&registry, "shipstern_transactions_parsing_errors"),
        0
    );
    assert_eq!(
        counter(&registry, "shipstern_transactions_handler_errors"),
        0
    );
    assert_eq!(
        counter(&registry, "shipstern_instructions_parsing_errors"),
        1
    );

    let successful_instructions = counter(&registry, "shipstern_instructions_successful");
    let successful_transactions = counter(&registry, "shipstern_transactions_successful");
    let instruction_parsing_errors = counter(&registry, "shipstern_instructions_parsing_errors");
    let instruction_handler_errors = counter(&registry, "shipstern_instructions_handler_errors");

    // A transaction of three instructions that hit every outcome, and one whose
    // instructions all succeed. A third transaction is filtered out by its parser.
    let mut filtered = transaction(&[]);

    filtered.slot = 2;

    Runtime::builder()
        .metrics(registry.clone())
        .transaction(Pipeline::new(FilteringTxParser, [NoopHandler]))
        .instruction(Pipeline::new(FilteringParser, [NoopHandler]))
        .instruction(Pipeline::new(FailingParser, [FailOnTagTwo]))
        .try_build_with(
            TransactionSource {
                transaction_parser: FILTERING_TX_PARSER,
                transactions: vec![transaction(&[1, 2, 3]), transaction(&[1]), filtered],
            },
            BufferConfig::default(),
        )
        .unwrap()
        .try_run_async()
        .await
        .unwrap();

    // Four instructions across two parsers is eight pairs. The filtered pair is
    // no success, and the failed pairs reach their own error counters. The
    // filtered transaction is no success either.
    assert_eq!(
        counter(&registry, "shipstern_instructions_successful") - successful_instructions,
        5
    );
    assert_eq!(
        counter(&registry, "shipstern_instructions_parsing_errors") - instruction_parsing_errors,
        1
    );
    assert_eq!(
        counter(&registry, "shipstern_instructions_handler_errors") - instruction_handler_errors,
        1
    );
    assert_eq!(
        counter(&registry, "shipstern_transactions_successful") - successful_transactions,
        2
    );

    let successful_instructions = counter(&registry, "shipstern_instructions_successful");
    let successful_transactions = counter(&registry, "shipstern_transactions_successful");

    // The same filtering parsers wrapped in a `FilterPipeline`. One instruction
    // and one transaction are filtered out.
    let mut filtered = transaction(&[]);

    filtered.slot = 2;

    Runtime::builder()
        .metrics(registry.clone())
        .transaction(FilterPipeline::new(
            FilteringTxParser,
            [NoopHandler],
            Prefilter::builder(),
        ))
        .instruction(FilterPipeline::new(
            FilteringParser,
            [NoopHandler],
            Prefilter::builder(),
        ))
        .try_build_with(
            TransactionSource {
                transaction_parser: FILTERING_TX_PARSER,
                transactions: vec![transaction(&[1, 2]), filtered],
            },
            BufferConfig::default(),
        )
        .unwrap()
        .try_run_async()
        .await
        .unwrap();

    assert_eq!(
        counter(&registry, "shipstern_instructions_successful") - successful_instructions,
        1
    );
    assert_eq!(
        counter(&registry, "shipstern_transactions_successful") - successful_transactions,
        1
    );
}
