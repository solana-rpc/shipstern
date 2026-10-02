//! Downstream Kafka writers that consume slots from the coordinator.
//!
//! `TransactionSlotSink` — receives `InstructionSlot<PreparedRecord>`, writes to `transaction.slots`.
//! `AccountSlotSink` — receives `AccountSlot<PreparedRecord>` from the coordinator and writes
//! to `account.slots`.
//! `AccountPassthroughSink` — receives `AccountMsg` directly and produces to Kafka immediately.

use std::{future::Future, sync::Arc, time::Duration};

use rdkafka::{
    error::KafkaError,
    message::OwnedHeaders,
    producer::{FutureProducer, FutureRecord, Producer},
};
#[cfg(feature = "experimental-account-parser")]
use shipstern_block_coordinator::AccountSlot;
use shipstern_block_coordinator::{AccountCommitAt, InstructionSlot};
use tokio::{sync::mpsc, time::sleep};

type SinkError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
enum TransactionCommitError {
    AbortRequired(SinkError),
    Failed(SinkError),
}

fn kafka_error(context: &str, err: impl std::fmt::Display) -> SinkError {
    format!("{context}: {err}").into()
}

fn kafka_send_error(context: &str, slot: u64, err: impl std::fmt::Display) -> SinkError {
    format!("{context} for slot {slot}: {err}").into()
}

fn transaction_error_context(err: &KafkaError) -> String {
    match err {
        KafkaError::Transaction(txn) => format!(
            "{} (code={}, retriable={}, abort_required={}, fatal={})",
            txn,
            txn.code(),
            txn.is_retriable(),
            txn.txn_requires_abort(),
            txn.is_fatal()
        ),
        _ => err.to_string(),
    }
}

async fn run_transactional_op<F>(
    producer: &FutureProducer,
    timeout: Duration,
    backoff: Duration,
    max_attempts: u32,
    context: &'static str,
    mut op: F,
) -> Result<(), SinkError>
where
    F: FnMut(&FutureProducer, Duration) -> Result<(), KafkaError>,
{
    let max_attempts = max_attempts.max(1);

    for attempt in 1..=max_attempts {
        match op(producer, timeout) {
            Ok(()) => return Ok(()),
            Err(KafkaError::Transaction(txn)) if txn.is_retriable() && attempt < max_attempts => {
                tracing::warn!(
                    attempt,
                    max_attempts,
                    code = ?txn.code(),
                    retriable = txn.is_retriable(),
                    abort_required = txn.txn_requires_abort(),
                    fatal = txn.is_fatal(),
                    "Kafka transactional operation failed, retrying"
                );

                sleep(backoff).await;
            },
            Err(err) => {
                return Err(kafka_error(context, transaction_error_context(&err)));
            },
        }
    }

    unreachable!("max_attempts >= 1")
}

async fn commit_transactional_op<F>(
    producer: &FutureProducer,
    timeout: Duration,
    backoff: Duration,
    max_attempts: u32,
    context: &'static str,
    mut op: F,
) -> Result<(), TransactionCommitError>
where
    F: FnMut(&FutureProducer, Duration) -> Result<(), KafkaError>,
{
    let max_attempts = max_attempts.max(1);

    for attempt in 1..=max_attempts {
        match op(producer, timeout) {
            Ok(()) => return Ok(()),
            Err(KafkaError::Transaction(txn)) if txn.txn_requires_abort() => {
                let err = KafkaError::Transaction(txn);

                return Err(TransactionCommitError::AbortRequired(kafka_error(
                    context,
                    transaction_error_context(&err),
                )));
            },
            Err(KafkaError::Transaction(txn)) if txn.is_retriable() && attempt < max_attempts => {
                tracing::warn!(
                    attempt,
                    max_attempts,
                    code = ?txn.code(),
                    retriable = txn.is_retriable(),
                    abort_required = txn.txn_requires_abort(),
                    fatal = txn.is_fatal(),
                    "Kafka transactional commit failed, retrying"
                );

                sleep(backoff).await;
            },
            Err(err) => {
                return Err(TransactionCommitError::Failed(kafka_error(
                    context,
                    transaction_error_context(&err),
                )));
            },
        }
    }

    unreachable!("max_attempts >= 1")
}

async fn with_retry<F, Fut, R>(
    max_attempts: u32,
    backoff: Duration,
    context: &str,
    slot: u64,
    mut f: F,
    mut on_retry: R,
) -> Result<(), SinkError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), SinkError>>,
    R: FnMut(&SinkError, u32, u32),
{
    let max_attempts = max_attempts.max(1);

    for attempt in 1..=max_attempts {
        match f().await {
            Ok(()) => return Ok(()),
            Err(e) if attempt < max_attempts => {
                on_retry(&e, attempt, max_attempts);

                sleep(backoff).await;
            },
            Err(e) => {
                return Err(format!(
                    "{context} for slot {slot} failed after {attempt} attempts: {e}"
                )
                .into());
            },
        }
    }
    unreachable!("max_attempts >= 1")
}

#[cfg(feature = "experimental-account-parser")]
use crate::events::{AccountSlotCommitEvent, MarkerType};
use crate::{
    config::KafkaSinkConfig,
    events::{CommitScope, PreparedRecord, RecordHeader, RecordKind, TransactionSlotCommitEvent},
};

fn to_kafka_headers(headers: &[RecordHeader]) -> OwnedHeaders {
    headers.iter().fold(OwnedHeaders::new(), |acc, h| {
        acc.insert(rdkafka::message::Header {
            key: h.key,
            value: Some(h.value.as_bytes()),
        })
    })
}

async fn batch_publish_records(
    producer: &FutureProducer,
    slot: u64,
    records: &[PreparedRecord],
    timeout: Duration,
    error_prefix: &'static str,
) -> Result<(), SinkError> {
    let futures: Vec<_> = records
        .iter()
        .map(|record| {
            let headers = to_kafka_headers(&record.headers);

            producer.send(
                FutureRecord::to(&record.topic)
                    .payload(&record.payload)
                    .key(&record.key)
                    .headers(headers),
                timeout,
            )
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    for (i, result) in results.into_iter().enumerate() {
        result.map_err(|(e, _)| {
            tracing::error!(
                ?e,
                slot,
                topic = %records[i].topic,
                error_prefix,
                "Kafka write failed"
            );

            kafka_send_error(error_prefix, slot, e)
        })?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct RetrySettings {
    max_attempts: u32,
    backoff: Duration,
}

impl RetrySettings {
    fn new(max_attempts: u32, backoff: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            backoff,
        }
    }

    fn get_write_policy_from_kafka_config(config: &KafkaSinkConfig) -> Self {
        Self::new(
            config.kafka_write_max_attempts,
            Duration::from_millis(config.kafka_retry_backoff_ms),
        )
    }

    fn get_transaction_write_policy_from_kafka_config(config: &KafkaSinkConfig) -> Self {
        Self::new(
            config.kafka_transaction_op_max_attempts,
            Duration::from_millis(config.kafka_retry_backoff_ms),
        )
    }
}

#[derive(Clone, Debug)]
enum DeliveryGuarantee {
    NonTransactional,
    Transactional {
        transactional_id: String,
        timeout: Duration,
    },
}

impl DeliveryGuarantee {
    fn from_kafka_config(config: &KafkaSinkConfig) -> Self {
        match &config.transactional_id {
            Some(transactional_id) => Self::Transactional {
                transactional_id: transactional_id.clone(),
                timeout: Duration::from_millis(config.transaction_timeout_ms.into()),
            },
            None => Self::NonTransactional,
        }
    }
}

#[derive(Debug)]
struct TransactionCheckpointLog {
    decoded_instruction_count: u64,
    decode_filtered_instruction_count: u64,
    decode_error_instruction_count: u64,
    fallback_instruction_count: u64,
    transaction_status_failed_count: u64,
    transaction_status_succeeded_count: u64,
}

#[cfg(feature = "experimental-account-parser")]
#[derive(Debug)]
struct AccountCheckpointLog {
    marker_type: MarkerType,
    account_commit_at: CommitScope,
    decoded_account_count: u64,
    decode_filtered_account_count: u64,
    decode_error_account_count: u64,
    fallback_account_count: u64,
}

#[derive(Debug)]
enum SlotCheckpointLog {
    Transaction(TransactionCheckpointLog),
    #[cfg(feature = "experimental-account-parser")]
    Account(AccountCheckpointLog),
}

impl SlotCheckpointLog {
    fn emit(&self, slot: u64, record_count: usize) {
        match self {
            Self::Transaction(log) => {
                tracing::debug!(
                    slot,
                    decoded_instruction_count = log.decoded_instruction_count,
                    decode_filtered_instruction_count = log.decode_filtered_instruction_count,
                    decode_error_instruction_count = log.decode_error_instruction_count,
                    fallback_instruction_count = log.fallback_instruction_count,
                    transaction_status_failed_count = log.transaction_status_failed_count,
                    transaction_status_succeeded_count = log.transaction_status_succeeded_count,
                    record_count,
                    "Kafka: published instruction slot checkpoint"
                );
            },
            #[cfg(feature = "experimental-account-parser")]
            Self::Account(log) => {
                tracing::debug!(
                    slot,
                    marker_type = %log.marker_type,
                    account_commit_at = %log.account_commit_at,
                    decoded_account_count = log.decoded_account_count,
                    decode_filtered_account_count = log.decode_filtered_account_count,
                    decode_error_account_count = log.decode_error_account_count,
                    fallback_account_count = log.fallback_account_count,
                    record_count,
                    "Kafka: published account slot checkpoint"
                );
            },
        }
    }
}

struct SlotCheckpointRecord<'a> {
    topic: &'a str,
    payload: String,
    error_prefix: &'static str,
    log: SlotCheckpointLog,
}

struct SlotWritePlan<'a> {
    slot: u64,
    records: &'a [PreparedRecord],
    record_error_prefix: &'static str,
    checkpoint: SlotCheckpointRecord<'a>,
}

impl SlotWritePlan<'_> {
    async fn publish_checkpoint(&self, producer: &FutureProducer) -> Result<(), SinkError> {
        let slot_key = self.slot.to_string();

        producer
            .send(
                FutureRecord::to(self.checkpoint.topic)
                    .payload(&self.checkpoint.payload)
                    .key(&slot_key),
                Duration::from_secs(5),
            )
            .await
            .map_err(|(e, _)| kafka_send_error(self.checkpoint.error_prefix, self.slot, e))?;

        self.checkpoint.log.emit(self.slot, self.records.len());
        Ok(())
    }
}

struct SlotWriteExecutor {
    producer: Arc<FutureProducer>,
    delivery: DeliveryGuarantee,
    write_retry_policy: RetrySettings,
    transaction_retry_policy: RetrySettings,
    transactions_initialized: bool,
}

impl SlotWriteExecutor {
    fn from_kafka_config(config: &KafkaSinkConfig, producer: Arc<FutureProducer>) -> Self {
        Self::from_kafka_config_with_initialized_transactions(config, producer, false)
    }

    fn from_kafka_config_with_initialized_transactions(
        config: &KafkaSinkConfig,
        producer: Arc<FutureProducer>,
        transactions_initialized: bool,
    ) -> Self {
        Self {
            producer,
            delivery: DeliveryGuarantee::from_kafka_config(config),
            write_retry_policy: RetrySettings::get_write_policy_from_kafka_config(config),
            transaction_retry_policy: RetrySettings::get_transaction_write_policy_from_kafka_config(
                config,
            ),
            transactions_initialized: transactions_initialized && config.transactional_id.is_some(),
        }
    }

    fn producer(&self) -> &FutureProducer { self.producer.as_ref() }

    fn write_retry_policy(&self) -> RetrySettings { self.write_retry_policy }

    async fn initialize(&self) -> Result<(), SinkError> {
        let DeliveryGuarantee::Transactional {
            transactional_id,
            timeout,
        } = &self.delivery
        else {
            return Ok(());
        };

        if self.transactions_initialized {
            tracing::debug!(
                transactional_id = %transactional_id,
                "Kafka transactions already initialized before sink startup"
            );

            return Ok(());
        }

        tracing::info!(
            transactional_id = %transactional_id,
            "Initializing Kafka transactions"
        );

        run_transactional_op(
            self.producer(),
            *timeout,
            self.transaction_retry_policy.backoff,
            self.transaction_retry_policy.max_attempts,
            "Failed to initialize Kafka transactions",
            |producer, timeout| producer.init_transactions(timeout),
        )
        .await
    }

    async fn execute(&self, plan: &SlotWritePlan<'_>) -> Result<(), SinkError> {
        match &self.delivery {
            DeliveryGuarantee::NonTransactional => self.publish_plan(plan).await,
            DeliveryGuarantee::Transactional { timeout, .. } => {
                self.execute_transactional(plan, *timeout).await
            },
        }
    }

    async fn publish_plan(&self, plan: &SlotWritePlan<'_>) -> Result<(), SinkError> {
        batch_publish_records(
            self.producer(),
            plan.slot,
            plan.records,
            Duration::ZERO,
            plan.record_error_prefix,
        )
        .await?;
        plan.publish_checkpoint(self.producer()).await
    }

    async fn execute_transactional(
        &self,
        plan: &SlotWritePlan<'_>,
        timeout: Duration,
    ) -> Result<(), SinkError> {
        // Two retry scopes on purpose: control ops retry locally on transient producer
        // state, and the slot-level retry replays the whole write after an abort, so
        // visibility stays atomic per slot.
        run_transactional_op(
            self.producer(),
            timeout,
            self.transaction_retry_policy.backoff,
            self.transaction_retry_policy.max_attempts,
            "Failed to begin Kafka transaction",
            |producer, _| producer.begin_transaction(),
        )
        .await?;

        let publish_result = self.publish_plan(plan).await;

        if let Err(err) = publish_result {
            return match self
                .abort_transaction(plan.slot, "Kafka transactional slot write failed", timeout)
                .await
            {
                Ok(()) => Err(err),
                Err(abort_err) => Err(format!(
                    "{err}; additionally failed to abort Kafka transaction: {abort_err}"
                )
                .into()),
            };
        }

        match commit_transactional_op(
            self.producer(),
            timeout,
            self.transaction_retry_policy.backoff,
            self.transaction_retry_policy.max_attempts,
            "Failed to commit Kafka transaction",
            |producer, timeout| producer.commit_transaction(timeout),
        )
        .await
        {
            Ok(()) => Ok(()),
            Err(TransactionCommitError::AbortRequired(err)) => {
                self.abort_transaction(
                    plan.slot,
                    "Kafka transaction commit requires abort",
                    timeout,
                )
                .await?;

                Err(err)
            },
            Err(TransactionCommitError::Failed(err)) => Err(err),
        }
    }

    async fn abort_transaction(
        &self,
        slot: u64,
        cause: &str,
        timeout: Duration,
    ) -> Result<(), SinkError> {
        run_transactional_op(
            self.producer(),
            timeout,
            self.transaction_retry_policy.backoff,
            self.transaction_retry_policy.max_attempts,
            "Failed to abort Kafka transaction",
            |producer, timeout| producer.abort_transaction(timeout),
        )
        .await
        .map_err(|abort_err| {
            format!(
                "{cause} for slot {slot}; additionally failed to abort Kafka transaction: \
                 {abort_err}"
            )
            .into()
        })
    }
}

/// Consumes instruction slots from the coordinator and writes them to Kafka.
pub struct TransactionSlotSink {
    executor: SlotWriteExecutor,
    transaction_slots_topic: String,
}

impl TransactionSlotSink {
    pub fn new(config: KafkaSinkConfig, producer: Arc<FutureProducer>) -> Self {
        Self {
            executor: SlotWriteExecutor::from_kafka_config(&config, producer),
            transaction_slots_topic: config.transaction_slots_topic,
        }
    }

    pub fn new_preinitialized(config: KafkaSinkConfig, producer: Arc<FutureProducer>) -> Self {
        Self {
            executor: SlotWriteExecutor::from_kafka_config_with_initialized_transactions(
                &config, producer, true,
            ),
            transaction_slots_topic: config.transaction_slots_topic,
        }
    }

    fn build_slot_write_plan<'a>(
        &'a self,
        ix_slot: &'a InstructionSlot<PreparedRecord>,
    ) -> Result<SlotWritePlan<'a>, SinkError> {
        let event = build_transaction_slot_commit_event(ix_slot);

        Ok(SlotWritePlan {
            slot: ix_slot.slot,
            records: &ix_slot.records,
            record_error_prefix: "Kafka write failed",
            checkpoint: SlotCheckpointRecord {
                topic: self.transaction_slots_topic.as_str(),
                payload: serde_json::to_string(&event)?,
                error_prefix: "Failed to publish slot checkpoint",
                log: SlotCheckpointLog::Transaction(TransactionCheckpointLog {
                    decoded_instruction_count: event.decoded_instruction_count,
                    decode_filtered_instruction_count: ix_slot.filtered_instruction_count,
                    decode_error_instruction_count: ix_slot.failed_instruction_count,
                    fallback_instruction_count: event.fallback_instruction_count,
                    transaction_status_failed_count: ix_slot.transaction_status_failed_count,
                    transaction_status_succeeded_count: ix_slot.transaction_status_succeeded_count,
                }),
            },
        })
    }

    pub async fn run(
        self,
        mut rx: mpsc::Receiver<InstructionSlot<PreparedRecord>>,
    ) -> Result<(), SinkError> {
        tracing::info!("TransactionSlotSink started, waiting for instruction slots...");
        self.executor.initialize().await?;
        let write_retry_policy = self.executor.write_retry_policy();

        while let Some(ix_slot) = rx.recv().await {
            with_retry(
                write_retry_policy.max_attempts,
                write_retry_policy.backoff,
                "Kafka write",
                ix_slot.slot,
                || async {
                    let plan = self.build_slot_write_plan(&ix_slot)?;

                    self.executor.execute(&plan).await
                },
                |e, attempt, max| {
                    tracing::warn!(
                        ?e,
                        slot = ix_slot.slot,
                        attempt,
                        max_attempts = max,
                        "Kafka write failed, retrying"
                    );
                },
            )
            .await?;
        }

        tracing::warn!("TransactionSlotSink channel closed, shutting down");
        Ok(())
    }
}

#[cfg(feature = "experimental-account-parser")]
/// Messages from the passthrough account pipeline.
pub enum AccountMsg {
    Record {
        slot: u64,
        record: Option<PreparedRecord>,
        had_error: bool,
    },
}

#[cfg(feature = "experimental-account-parser")]
/// Writes buffered account slots to Kafka and publishes account slot checkpoints.
pub struct AccountSlotSink {
    executor: SlotWriteExecutor,
    account_slots_topic: String,
    account_commit_scope: CommitScope,
}

#[cfg(feature = "experimental-account-parser")]
impl AccountSlotSink {
    pub fn new(
        config: KafkaSinkConfig,
        producer: Arc<FutureProducer>,
        account_commit_at: AccountCommitAt,
    ) -> Self {
        Self {
            executor: SlotWriteExecutor::from_kafka_config(&config, producer),
            account_slots_topic: config.account_slots_topic,
            account_commit_scope: account_commit_at.into(),
        }
    }

    pub fn new_preinitialized(
        config: KafkaSinkConfig,
        producer: Arc<FutureProducer>,
        account_commit_at: AccountCommitAt,
    ) -> Self {
        Self {
            executor: SlotWriteExecutor::from_kafka_config_with_initialized_transactions(
                &config, producer, true,
            ),
            account_slots_topic: config.account_slots_topic,
            account_commit_scope: account_commit_at.into(),
        }
    }

    fn build_slot_write_plan<'a>(
        &'a self,
        acct_slot: &'a AccountSlot<PreparedRecord>,
    ) -> Result<SlotWritePlan<'a>, SinkError> {
        let event = build_account_slot_commit_event(acct_slot, self.account_commit_scope);

        Ok(SlotWritePlan {
            slot: acct_slot.slot,
            records: &acct_slot.records,
            record_error_prefix: "Account Kafka write failed",
            checkpoint: SlotCheckpointRecord {
                topic: self.account_slots_topic.as_str(),
                payload: serde_json::to_string(&event)?,
                error_prefix: "Failed to publish account slot checkpoint",
                log: SlotCheckpointLog::Account(AccountCheckpointLog {
                    marker_type: event.marker_type,
                    account_commit_at: event.account_commit_at,
                    decoded_account_count: event.decoded_account_count.unwrap_or(0),
                    decode_filtered_account_count: event.decode_filtered_account_count.unwrap_or(0),
                    decode_error_account_count: event.decode_error_account_count.unwrap_or(0),
                    fallback_account_count: event.fallback_account_count.unwrap_or(0),
                }),
            },
        })
    }

    pub async fn run(
        self,
        mut rx: mpsc::Receiver<AccountSlot<PreparedRecord>>,
    ) -> Result<(), SinkError> {
        tracing::info!("AccountSlotSink started");
        self.executor.initialize().await?;
        let write_retry_policy = self.executor.write_retry_policy();

        while let Some(acct_slot) = rx.recv().await {
            with_retry(
                write_retry_policy.max_attempts,
                write_retry_policy.backoff,
                "Account slot write",
                acct_slot.slot,
                || async {
                    let plan = self.build_slot_write_plan(&acct_slot)?;

                    self.executor.execute(&plan).await
                },
                |e, attempt, max| {
                    tracing::warn!(
                        ?e,
                        slot = acct_slot.slot,
                        attempt,
                        max_attempts = max,
                        "Account slot write failed, retrying"
                    );
                },
            )
            .await?;
        }

        tracing::warn!("AccountSlotSink channel closed, shutting down");
        Ok(())
    }
}

#[cfg(feature = "experimental-account-parser")]
/// Produces account records immediately and advances a monotonic watermark by slot.
pub struct AccountPassthroughSink {
    producer: Arc<FutureProducer>,
    account_slots_topic: String,
    retry_settings: RetrySettings,
}

#[cfg(feature = "experimental-account-parser")]
impl AccountPassthroughSink {
    /// Construct the non-transactional passthrough sink.
    ///
    /// Passthrough writes account records immediately and emits watermarks later,
    /// so it only needs the producer, slot topic, and write retry settings.
    pub fn new(
        producer: Arc<FutureProducer>,
        account_slots_topic: String,
        max_attempts: u32,
        retry_backoff_ms: u64,
    ) -> Self {
        Self {
            producer,
            account_slots_topic,
            retry_settings: RetrySettings::new(
                max_attempts,
                Duration::from_millis(retry_backoff_ms),
            ),
        }
    }

    pub async fn run(self, mut rx: mpsc::Receiver<AccountMsg>) -> Result<(), SinkError> {
        tracing::info!("AccountPassthroughSink started");

        let mut current_slot: Option<u64> = None;

        while let Some(msg) = rx.recv().await {
            match msg {
                AccountMsg::Record { slot, record, .. } => {
                    if let Some(record) = record {
                        with_retry(
                            self.retry_settings.max_attempts,
                            self.retry_settings.backoff,
                            "Passthrough account write",
                            slot,
                            || self.publish_passthrough_record(slot, &record),
                            |e, attempt, max| {
                                tracing::warn!(
                                    ?e,
                                    slot,
                                    attempt,
                                    max_attempts = max,
                                    topic = %record.topic,
                                    "Passthrough account write failed, retrying"
                                );
                            },
                        )
                        .await?;
                    }

                    if let Some(prev) = current_slot {
                        if slot > prev {
                            self.emit_watermark_with_retry(prev).await?;

                            current_slot = Some(slot);
                        }
                    } else {
                        current_slot = Some(slot);
                    }
                },
            }
        }

        if let Some(slot) = current_slot {
            self.emit_watermark_with_retry(slot).await?;
        }

        tracing::warn!("AccountPassthroughSink channel closed, shutting down");
        Ok(())
    }

    async fn publish_passthrough_record(
        &self,
        slot: u64,
        record: &PreparedRecord,
    ) -> Result<(), SinkError> {
        let headers = to_kafka_headers(&record.headers);
        self.producer
            .send(
                FutureRecord::to(&record.topic)
                    .payload(&record.payload)
                    .key(&record.key)
                    .headers(headers),
                Duration::from_secs(5),
            )
            .await
            .map_err(|(e, _)| kafka_send_error("Passthrough Kafka write failed", slot, e))?;
        Ok(())
    }

    async fn emit_watermark_with_retry(&self, slot: u64) -> Result<(), SinkError> {
        with_retry(
            self.retry_settings.max_attempts,
            self.retry_settings.backoff,
            "Emit account watermark",
            slot,
            || self.emit_watermark(slot),
            |e, attempt, max| {
                tracing::warn!(
                    ?e,
                    slot,
                    attempt,
                    max_attempts = max,
                    "Failed to emit account watermark, retrying"
                );
            },
        )
        .await
    }

    async fn emit_watermark(&self, slot: u64) -> Result<(), SinkError> {
        let event = AccountSlotCommitEvent {
            slot,
            marker_type: MarkerType::Watermark,
            account_commit_at: CommitScope::Stream,
            decoded_account_count: None,
            decode_filtered_account_count: None,
            decode_error_account_count: None,
            fallback_account_count: None,
        };

        let payload = serde_json::to_string(&event)?;

        let slot_key = slot.to_string();

        self.producer
            .send(
                FutureRecord::to(&self.account_slots_topic)
                    .payload(&payload)
                    .key(&slot_key),
                Duration::from_secs(5),
            )
            .await
            .map_err(|(e, _)| kafka_send_error("Failed to emit watermark", slot, e))?;

        tracing::debug!(slot, "Emitted account watermark");

        Ok(())
    }
}

fn build_transaction_slot_commit_event(
    ix_slot: &InstructionSlot<PreparedRecord>,
) -> TransactionSlotCommitEvent {
    let decoded_instruction_count = ix_slot
        .records
        .iter()
        .filter(|r| r.is_decoded && r.kind == RecordKind::Instruction)
        .count() as u64;

    let fallback_instruction_count = ix_slot
        .records
        .iter()
        .filter(|r| !r.is_decoded && r.kind == RecordKind::Instruction)
        .count() as u64;

    TransactionSlotCommitEvent {
        slot: ix_slot.slot,
        blockhash: ix_slot.blockhash.to_string(),
        transaction_count: ix_slot.executed_transaction_count,
        decoded_instruction_count,
        decode_filtered_instruction_count: ix_slot.filtered_instruction_count,
        decode_error_instruction_count: ix_slot.failed_instruction_count,
        fallback_instruction_count,
        transaction_status_failed_count: ix_slot.transaction_status_failed_count,
        transaction_status_succeeded_count: ix_slot.transaction_status_succeeded_count,
    }
}

#[cfg(feature = "experimental-account-parser")]
fn build_account_slot_commit_event(
    acct_slot: &AccountSlot<PreparedRecord>,
    account_commit_scope: CommitScope,
) -> AccountSlotCommitEvent {
    let decoded_account_count = acct_slot
        .records
        .iter()
        .filter(|r| r.is_decoded && r.kind == RecordKind::Account)
        .count() as u64;
    let fallback_account_count = acct_slot
        .records
        .iter()
        .filter(|r| !r.is_decoded && r.kind == RecordKind::Account)
        .count() as u64;

    AccountSlotCommitEvent {
        slot: acct_slot.slot,
        marker_type: MarkerType::Completed,
        account_commit_at: account_commit_scope,
        decoded_account_count: Some(decoded_account_count),
        decode_filtered_account_count: Some(acct_slot.filtered_account_count),
        decode_error_account_count: Some(acct_slot.failed_account_count),
        fallback_account_count: Some(fallback_account_count),
    }
}

impl From<AccountCommitAt> for CommitScope {
    fn from(commit_at: AccountCommitAt) -> Self {
        match commit_at {
            AccountCommitAt::Confirmed => Self::Confirmed,
            AccountCommitAt::Finalized => Self::Finalized,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "experimental-account-parser")]
    use shipstern_block_coordinator::AccountSlot;
    use shipstern_block_coordinator::InstructionSlot;

    use super::*;
    use crate::events::{AccountSlotCommitEvent, MarkerType};

    fn record(topic: &str, kind: RecordKind, is_decoded: bool) -> PreparedRecord {
        PreparedRecord {
            topic: topic.to_string(),
            payload: vec![],
            key: "k".to_string(),
            headers: vec![],
            is_decoded,
            kind,
        }
    }

    #[test]
    fn transaction_commit_event_counts_decode_and_fallback_explicitly() {
        let ix_slot = InstructionSlot {
            slot: 42,
            parent_slot: 41,
            blockhash: Default::default(),
            executed_transaction_count: 10,
            records: vec![
                record("decoded.instructions", RecordKind::Instruction, true),
                record("failed.instructions", RecordKind::Instruction, false),
                // Defensive: non-instruction record must not affect instruction counters.
                record("decoded.accounts", RecordKind::Account, true),
            ],
            filtered_instruction_count: 7,
            failed_instruction_count: 3,
            transaction_status_failed_count: 2,
            transaction_status_succeeded_count: 8,
        };

        let event = build_transaction_slot_commit_event(&ix_slot);

        assert_eq!(event.slot, 42);
        assert_eq!(event.transaction_count, 10);
        assert_eq!(event.decoded_instruction_count, 1);
        assert_eq!(event.decode_filtered_instruction_count, 7);
        assert_eq!(event.decode_error_instruction_count, 3);
        assert_eq!(event.fallback_instruction_count, 1);
        assert_eq!(event.transaction_status_failed_count, 2);
        assert_eq!(event.transaction_status_succeeded_count, 8);
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn account_commit_event_counts_decode_and_fallback_explicitly() {
        let acct_slot = AccountSlot {
            slot: 55,
            records: vec![
                record("decoded.accounts", RecordKind::Account, true),
                record("failed.accounts", RecordKind::Account, false),
                // Defensive: non-account record must not affect account counters.
                record("decoded.instructions", RecordKind::Instruction, true),
            ],
            decoded_account_count: 0, // event computes this from records, not this field.
            filtered_account_count: 4,
            failed_account_count: 2,
        };

        let event = build_account_slot_commit_event(&acct_slot, CommitScope::Confirmed);

        assert_eq!(event.slot, 55);
        assert_eq!(event.marker_type, MarkerType::Completed);
        assert_eq!(event.account_commit_at, CommitScope::Confirmed);
        assert_eq!(event.decoded_account_count, Some(1));
        assert_eq!(event.decode_filtered_account_count, Some(4));
        assert_eq!(event.decode_error_account_count, Some(2));
        assert_eq!(event.fallback_account_count, Some(1));
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn passthrough_mode_commits_as_finalized() {
        let acct_slot = AccountSlot {
            slot: 99,
            records: vec![],
            decoded_account_count: 0,
            filtered_account_count: 0,
            failed_account_count: 0,
        };

        let event = build_account_slot_commit_event(&acct_slot, CommitScope::Finalized);

        assert_eq!(event.marker_type, MarkerType::Completed);
        assert_eq!(event.account_commit_at, CommitScope::Finalized);
    }

    #[test]
    fn watermark_json_omits_account_count_fields() {
        let event = AccountSlotCommitEvent {
            slot: 77,
            marker_type: MarkerType::Watermark,
            account_commit_at: CommitScope::Stream,
            decoded_account_count: None,
            decode_filtered_account_count: None,
            decode_error_account_count: None,
            fallback_account_count: None,
        };

        let value = serde_json::to_value(&event).expect("serialize watermark");

        let obj = value.as_object().expect("object");

        assert!(!obj.contains_key("decoded_account_count"));
        assert!(!obj.contains_key("decode_filtered_account_count"));
        assert!(!obj.contains_key("decode_error_account_count"));
        assert!(!obj.contains_key("fallback_account_count"));
    }

    #[cfg(feature = "experimental-account-parser")]
    #[test]
    fn completed_json_includes_account_count_fields() {
        let acct_slot = AccountSlot {
            slot: 88,
            records: vec![
                record("decoded.accounts", RecordKind::Account, true),
                record("failed.accounts", RecordKind::Account, false),
            ],
            decoded_account_count: 0,
            filtered_account_count: 9,
            failed_account_count: 3,
        };

        let event = build_account_slot_commit_event(&acct_slot, CommitScope::Confirmed);

        let value = serde_json::to_value(&event).expect("serialize completed marker");
        let obj = value.as_object().expect("object");

        assert_eq!(
            obj.get("decoded_account_count").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            obj.get("decode_filtered_account_count")
                .and_then(|v| v.as_u64()),
            Some(9)
        );
        assert_eq!(
            obj.get("decode_error_account_count")
                .and_then(|v| v.as_u64()),
            Some(3)
        );
        assert_eq!(
            obj.get("fallback_account_count").and_then(|v| v.as_u64()),
            Some(1)
        );
    }
}
