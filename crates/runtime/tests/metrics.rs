//! Runs transactions through a runtime and reads the counters back from its
//! prometheus registry. The counters are process-wide, so this is the only test
//! in its binary.

use std::borrow::Cow;

use async_trait::async_trait;
use prometheus::Registry;
use shipstern::{
    config::BufferConfig,
    instruction::InstructionPipeline,
    shipstern_core::{
        instruction::InstructionUpdate, ParseResult, Parser, Prefilter, TransactionUpdate,
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

#[derive(Debug, Clone, Copy)]
struct NoopHandler;

impl Handler<(), TransactionUpdate> for NoopHandler {
    async fn handle(&self, (): &(), _: &TransactionUpdate) -> HandlerResult<()> { Ok(()) }
}

impl Handler<(), InstructionUpdate> for NoopHandler {
    async fn handle(&self, (): &(), _: &InstructionUpdate) -> HandlerResult<()> { Ok(()) }
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
struct OneTransactionSource;

#[async_trait]
impl SourceTrait for OneTransactionSource {
    async fn connect(&self, ctx: SourceContext) -> Result<SourceExitStatus, Error> {
        let SourceContext { tx, .. } = ctx;

        // Two transactions whose instructions all succeed, and one whose
        // instruction tree cannot be built.
        let mut malformed = transaction(&[1]);

        malformed.transaction.as_mut().unwrap().meta = None;

        for transaction in [transaction(&[1, 2]), transaction(&[1]), malformed] {
            let update = SubscribeUpdate {
                filters: vec![TX_PARSER.to_owned(), InstructionPipeline::ID.to_owned()],
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
async fn instruction_pipelines_do_not_count_as_transactions() {
    let registry = Registry::new();

    Runtime::builder()
        .metrics(registry.clone())
        .transaction(Pipeline::new(TxParser, [NoopHandler]))
        .instruction(Pipeline::new(InstructionParser, [NoopHandler]))
        .try_build_with(OneTransactionSource, BufferConfig::default())
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
}
