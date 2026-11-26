use std::{thread, time::Duration};

use rdkafka::{
    error::KafkaError,
    producer::{FutureProducer, Producer},
    types::RDKafkaErrorCode,
    ClientConfig,
};

use crate::config::KafkaSinkConfig;

/// Create the Kafka producer for `config`.
///
/// # Panics
///
/// Panics if `config` fails [`KafkaSinkConfig::validate_credentials`] or the
/// client cannot be created.
pub fn create_producer(config: &KafkaSinkConfig) -> FutureProducer {
    if let Err(e) = config.validate_credentials() {
        panic!("Invalid Kafka sink config: {e}");
    }

    let mut client_config = ClientConfig::new();

    client_config
        .set("bootstrap.servers", &config.brokers)
        .set("message.timeout.ms", config.message_timeout_ms.to_string())
        .set(
            "queue.buffering.max.messages",
            config.queue_buffering_max_messages.to_string(),
        )
        .set("batch.num.messages", config.batch_num_messages.to_string())
        .set("compression.type", config.compression_type.as_str())
        .set("enable.idempotence", "true");

    if let Some(transactional_id) = &config.transactional_id {
        client_config.set("transactional.id", transactional_id).set(
            "transaction.timeout.ms",
            config.transaction_timeout_ms.to_string(),
        );
    }

    config.apply_sasl_if_configured(&mut client_config);

    client_config
        .create()
        .expect("Failed to create Kafka producer")
}

fn transactional_error_context(error: &KafkaError) -> String {
    match error {
        KafkaError::Transaction(txn) => format!(
            "{} (code={:?}, retriable={}, abort_required={}, fatal={})",
            txn,
            txn.code(),
            txn.is_retriable(),
            txn.txn_requires_abort(),
            txn.is_fatal()
        ),
        other => other.to_string(),
    }
}

fn is_already_initialized_transaction_error(error: &KafkaError) -> bool {
    matches!(
        error,
        KafkaError::Transaction(txn) if txn.code() == RDKafkaErrorCode::State
    )
}

/// Initialize a transactional Kafka producer before the caller reads its resume checkpoint.
///
/// This fences any previous producer instance with the same `transactional.id` first,
/// so the caller can trust a subsequent `read_committed` checkpoint read as the latest
/// visible progress for that logical writer.
pub fn initialize_transactional_producer(
    config: &KafkaSinkConfig,
    producer: &FutureProducer,
) -> Result<(), String> {
    if config.transactional_id.is_none() {
        return Ok(());
    }

    let timeout = Duration::from_millis(config.transaction_timeout_ms.into());
    let backoff = Duration::from_millis(config.kafka_retry_backoff_ms);
    let max_attempts = config.kafka_transaction_op_max_attempts.max(1);

    for attempt in 1..=max_attempts {
        match producer.init_transactions(timeout) {
            Ok(()) => return Ok(()),
            Err(err) if is_already_initialized_transaction_error(&err) => return Ok(()),
            Err(KafkaError::Transaction(txn)) if txn.is_retriable() && attempt < max_attempts => {
                tracing::warn!(
                    transactional_id = config.transactional_id.as_deref(),
                    attempt,
                    max_attempts,
                    code = ?txn.code(),
                    retriable = txn.is_retriable(),
                    abort_required = txn.txn_requires_abort(),
                    fatal = txn.is_fatal(),
                    "Failed to initialize Kafka transactions before checkpoint read, retrying"
                );

                thread::sleep(backoff);
            },
            Err(err) => {
                return Err(format!(
                    "Failed to initialize Kafka transactions before checkpoint read: {}",
                    transactional_error_context(&err)
                ));
            },
        }
    }

    unreachable!("max_attempts is clamped to at least 1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "KAFKA_SASL_PASSWORD is missing")]
    fn create_producer_rejects_half_set_sasl_credentials() {
        let config = KafkaSinkConfig {
            sasl_username: Some("user".to_string()),
            ..Default::default()
        };

        let _producer = create_producer(&config);
    }
}
