//! Helper types for parsing and dispatching instructions from transaction
//! updates.

use std::fmt::{self, Debug};

use shipstern_core::{instruction::InstructionUpdate, GetPrefilter, ParserId, TransactionUpdate};

use crate::handler::{BoxPipeline, DynPipeline, PipelineErrors};
#[cfg(feature = "prometheus")]
use crate::metrics;

/// Builds the instruction tree for a transaction once and dispatches every
/// instruction to every bundled sub-pipeline.
///
/// Parsers self-filter by program id and return `ParseError::Filtered` for
/// foreign instructions, so dispatch is `O(instructions x parsers)`. Indexing by
/// [`ProgramParser::program_id`](shipstern_core::ProgramParser::program_id) would cut that.
///
pub struct InstructionPipeline(Box<[BoxPipeline<'static, InstructionUpdate>]>);

impl fmt::Debug for InstructionPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("InstructionPipeline").field(&self.0).finish()
    }
}

impl InstructionPipeline {
    /// The parser ID every bundled instruction parser shares.
    ///
    /// Because the runtime collapses them into one pipeline, the runtime's
    /// filter set carries a single entry under this ID whose prefilter is the
    /// union of all of them. A filter update naming an individual instruction
    /// parser is refused as unknown; key it here instead.
    ///
    /// ```rust, ignore
    /// handle.update_filters(|filters| {
    ///     filters.merge(InstructionPipeline::ID, extra);
    /// })?;
    /// ```
    ///
    pub const ID: &'static str = "InstructionPipeline";

    /// Create a new instruction pipeline from a list of sub-pipelines.
    #[must_use]
    pub fn new(pipelines: Vec<BoxPipeline<'static, InstructionUpdate>>) -> Option<Self> {
        if pipelines.is_empty() {
            return None;
        }

        Some(Self(pipelines.into_boxed_slice()))
    }

    /// Handle a transaction update by dispatching its instruction updates to
    /// the sub-pipelines.
    ///
    /// # Errors
    /// Returns an error if any of the sub-pipelines return an error.
    pub async fn handle(&self, txn: &TransactionUpdate) -> Result<(), PipelineErrors> {
        let mut err = None;
        let ixs = InstructionUpdate::build_from_txn(txn).map_err(PipelineErrors::parse)?;

        // Fan every instruction (walking the full CPI tree) out to every parser;
        // parsers self-filter by program id via ParseError::Filtered. See the
        // type-level docs for cost characteristics and the indexing follow-up.
        for insn in ixs.iter().flat_map(|i| i.visit_all()) {
            for pipe in &*self.0 {
                let res = pipe.handle(insn).await;

                #[cfg(feature = "prometheus")]
                metrics::increment_processed_updates(&res, metrics::UpdateType::Instruction);

                match res {
                    Ok(()) => (),
                    Err(PipelineErrors::AlreadyHandled(h)) => h.as_unit(),
                    Err(e) => {
                        let handled = e.handle::<InstructionUpdate>(&pipe.id());

                        if err.is_some() {
                            tracing::warn!(
                                pipeline = %pipe.id(),
                                "replacing a previous unreported pipeline error; \
                                 the earlier failure is being dropped"
                            );
                        }

                        err = Some(handled);
                    },
                }
            }
        }

        if let Some(h) = err {
            Err(PipelineErrors::AlreadyHandled(h))
        } else {
            Ok(())
        }
    }
}

impl ParserId for InstructionPipeline {
    fn id(&self) -> std::borrow::Cow<'static, str> { Self::ID.into() }
}

impl GetPrefilter for InstructionPipeline {
    fn prefilter(&self) -> shipstern_core::Prefilter {
        self.0.iter().map(GetPrefilter::prefilter).collect()
    }
}

impl DynPipeline<TransactionUpdate> for InstructionPipeline {
    fn handle<'h>(
        &'h self,
        value: &'h TransactionUpdate,
    ) -> std::pin::Pin<Box<dyn futures_util::Future<Output = Result<(), PipelineErrors>> + Send + 'h>>
    {
        Box::pin(InstructionPipeline::handle(self, value))
    }
}

/// Dispatches instruction updates to a single parser, rebuilding the tree on
/// each call. The runtime uses [`InstructionPipeline`]; this stays as the public
/// single-parser entry point.
///
pub struct SingleInstructionPipeline(BoxPipeline<'static, InstructionUpdate>);

impl SingleInstructionPipeline {
    /// Create a new instruction pipeline from a single sub-pipeline.
    #[must_use]
    pub fn new(pipeline: BoxPipeline<'static, InstructionUpdate>) -> Self { Self(pipeline) }

    /// Handle a transaction update by dispatching its instruction updates to
    /// its sub-pipeline.
    ///
    /// # Errors
    /// Returns an error if the inner pipeline fails.
    pub async fn handle(&self, txn: &TransactionUpdate) -> Result<(), PipelineErrors> {
        let mut err = None;
        let ixs = InstructionUpdate::build_from_txn(txn).map_err(PipelineErrors::parse)?;
        let pipe = &self.0;

        for insn in ixs.iter().flat_map(|i| i.visit_all()) {
            let res = pipe.handle(insn).await;

            #[cfg(feature = "prometheus")]
            metrics::increment_processed_updates(&res, metrics::UpdateType::Instruction);

            match res {
                Ok(()) => (),
                Err(PipelineErrors::AlreadyHandled(h)) => h.as_unit(),
                Err(e) => err = Some(e.handle::<InstructionUpdate>(&pipe.id())),
            }
        }

        if let Some(h) = err {
            Err(PipelineErrors::AlreadyHandled(h))
        } else {
            Ok(())
        }
    }
}

impl ParserId for SingleInstructionPipeline {
    fn id(&self) -> std::borrow::Cow<'static, str> { self.0.id() }
}

impl GetPrefilter for SingleInstructionPipeline {
    fn prefilter(&self) -> shipstern_core::Prefilter { self.0.prefilter() }
}

impl Debug for SingleInstructionPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SingleInstructionPipeline")
            .field(&self.0)
            .finish()
    }
}

impl DynPipeline<TransactionUpdate> for SingleInstructionPipeline {
    fn handle<'h>(
        &'h self,
        value: &'h TransactionUpdate,
    ) -> std::pin::Pin<Box<dyn futures_util::Future<Output = Result<(), PipelineErrors>> + Send + 'h>>
    {
        Box::pin(SingleInstructionPipeline::handle(self, value))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use shipstern_core::{
        instruction::InstructionUpdate, GetPrefilter, ParseError, ParseResult, Parser, Prefilter,
        Pubkey, TransactionUpdate,
    };
    use yellowstone_grpc_proto::{
        geyser::SubscribeUpdateTransactionInfo,
        solana::storage::confirmed_block::{
            CompiledInstruction, Message, MessageHeader, Transaction, TransactionStatusMeta,
        },
    };

    use super::{InstructionPipeline, PipelineErrors, SingleInstructionPipeline};
    use crate::handler::{BoxPipeline, Handler, HandlerResult, Pipeline};

    static HANDLED: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, Clone, Copy)]
    struct FailFirstIxParser;

    impl Parser for FailFirstIxParser {
        type Input = InstructionUpdate;
        // `Parser::Output` must implement `prost::Message`; `()` is prost's empty
        // message and is all this test needs (it only asserts parse success/failure).
        type Output = ();

        fn id(&self) -> Cow<'static, str> { "test::FailFirstIxParser".into() }

        fn prefilter(&self) -> Prefilter { Prefilter::default() }

        async fn parse(&self, value: &Self::Input) -> ParseResult<Self::Output> {
            if value.data.first() == Some(&0) {
                return Err(ParseError::Other("simulated unpack failure".into()));
            }

            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct CountingHandler;

    impl Handler<(), InstructionUpdate> for CountingHandler {
        async fn handle(&self, _value: &(), _raw: &InstructionUpdate) -> HandlerResult<()> {
            HANDLED.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn sample_transaction(instructions: Vec<CompiledInstruction>) -> TransactionUpdate {
        let program = [7u8; 32];
        let payer = [8u8; 32];
        let mint = [9u8; 32];

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
                        account_keys: vec![program.to_vec(), payer.to_vec(), mint.to_vec()],
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

    #[tokio::test]
    async fn single_instruction_pipeline_keeps_iterating_after_parse_error() {
        HANDLED.store(0, Ordering::Relaxed);

        let txn = sample_transaction(vec![
            CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1, 2],
                data: vec![0],
            },
            CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1, 2],
                data: vec![1],
            },
        ]);

        let pipeline: BoxPipeline<'static, InstructionUpdate> =
            Box::new(Pipeline::new(FailFirstIxParser, [CountingHandler]));
        let single = SingleInstructionPipeline::new(pipeline);

        let result = single.handle(&txn).await;

        assert!(matches!(result, Err(PipelineErrors::AlreadyHandled(_))));
        assert_eq!(
            HANDLED.load(Ordering::Relaxed),
            1,
            "handler should still run for instructions after a parse failure"
        );
    }

    static A_COUNT: AtomicUsize = AtomicUsize::new(0);
    static B_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug, Clone, Copy)]
    struct OkParser;

    impl Parser for OkParser {
        type Input = InstructionUpdate;
        type Output = ();

        fn id(&self) -> Cow<'static, str> { "test::OkParser".into() }

        fn prefilter(&self) -> Prefilter { Prefilter::default() }

        async fn parse(&self, _value: &Self::Input) -> ParseResult<Self::Output> { Ok(()) }
    }

    #[derive(Debug, Clone, Copy)]
    struct HandlerA;

    impl Handler<(), InstructionUpdate> for HandlerA {
        async fn handle(&self, _value: &(), _raw: &InstructionUpdate) -> HandlerResult<()> {
            A_COUNT.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct HandlerB;

    impl Handler<(), InstructionUpdate> for HandlerB {
        async fn handle(&self, _value: &(), _raw: &InstructionUpdate) -> HandlerResult<()> {
            B_COUNT.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[tokio::test]
    async fn instruction_pipeline_fans_out_to_all_bundled_parsers() {
        // Two parsers over a two-instruction transaction each handle both
        // instructions from one dispatch.
        A_COUNT.store(0, Ordering::Relaxed);
        B_COUNT.store(0, Ordering::Relaxed);

        let txn = sample_transaction(vec![
            CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1, 2],
                data: vec![1],
            },
            CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1, 2],
                data: vec![2],
            },
        ]);

        let a: BoxPipeline<'static, InstructionUpdate> =
            Box::new(Pipeline::new(OkParser, [HandlerA]));
        let b: BoxPipeline<'static, InstructionUpdate> =
            Box::new(Pipeline::new(OkParser, [HandlerB]));
        let bundle = InstructionPipeline::new(vec![a, b]).expect("non-empty pipeline");

        bundle.handle(&txn).await.expect("handle should succeed");

        assert_eq!(
            A_COUNT.load(Ordering::Relaxed),
            2,
            "parser A should see both instructions"
        );
        assert_eq!(
            B_COUNT.load(Ordering::Relaxed),
            2,
            "parser B should see both instructions"
        );
    }

    const PROGRAM_A: [u8; 32] = [10u8; 32];
    const PROGRAM_B: [u8; 32] = [20u8; 32];

    /// A parser that filters on a single program, used to check that bundling
    /// preserves each sub-parser's prefilter.
    #[derive(Debug, Clone, Copy)]
    struct ProgramParser {
        program: [u8; 32],
        id: &'static str,
    }

    impl Parser for ProgramParser {
        type Input = InstructionUpdate;
        type Output = ();

        fn id(&self) -> Cow<'static, str> { self.id.into() }

        fn prefilter(&self) -> Prefilter {
            Prefilter::builder()
                .transaction_accounts_include([self.program])
                .build()
                .expect("valid prefilter")
        }

        async fn parse(&self, _value: &Self::Input) -> ParseResult<Self::Output> { Ok(()) }
    }

    #[test]
    fn instruction_pipeline_merges_sub_parser_prefilters() {
        // The bundle must advertise the union of every parser's prefilter, or one
        // parser's program goes unsubscribed.
        let a: BoxPipeline<'static, InstructionUpdate> = Box::new(Pipeline::new(
            ProgramParser {
                program: PROGRAM_A,
                id: "test::ProgramA",
            },
            [CountingHandler],
        ));
        let b: BoxPipeline<'static, InstructionUpdate> = Box::new(Pipeline::new(
            ProgramParser {
                program: PROGRAM_B,
                id: "test::ProgramB",
            },
            [CountingHandler],
        ));

        let bundle = InstructionPipeline::new(vec![a, b]).expect("non-empty pipeline");

        let prefilter = bundle.prefilter();
        let txn = prefilter
            .transaction
            .expect("bundled prefilter should carry a transaction filter");

        assert!(
            txn.accounts_include.contains(&Pubkey::from(PROGRAM_A)),
            "union must include program A"
        );
        assert!(
            txn.accounts_include.contains(&Pubkey::from(PROGRAM_B)),
            "union must include program B"
        );
        assert_eq!(
            txn.accounts_include.len(),
            2,
            "union must contain exactly both programs (merge must not drop either)"
        );
    }
}
