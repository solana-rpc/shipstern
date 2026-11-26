use std::time::{Duration, Instant};

use rdkafka::{
    consumer::{BaseConsumer, Consumer},
    ClientConfig, Message, TopicPartitionList,
};

use crate::{config::KafkaSinkConfig, events::TransactionSlotCommitEvent};

const MAX_STARTUP_CHECKPOINT_SCAN_OFFSETS: i64 = 256;
const STARTUP_CHECKPOINT_SCAN_DEADLINE: Duration = Duration::from_secs(5);

/// Last committed block info from a slots topic.
#[derive(Debug, Clone, Copy)]
pub struct LastCommitted {
    pub slot: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LastCommittedCandidate {
    slot: u64,
    offset: i64,
}

/// Read the latest committed transaction slot from a topic for resumption.
/// Returns None if the topic is empty or doesn't exist.
pub fn read_last_committed_transaction_block(
    config: &KafkaSinkConfig,
) -> Result<Option<LastCommitted>, String> {
    read_last_slot_from_topic(config, &config.transaction_slots_topic)
}

/// Read the latest committed account slot for resumption.
/// Returns None if the topic is empty or doesn't exist.
pub fn read_last_committed_account_block(
    config: &KafkaSinkConfig,
) -> Result<Option<LastCommitted>, String> {
    read_last_slot_from_topic(config, &config.account_slots_topic)
}

/// Read the last committed slot from a Kafka topic by scanning backward from the tail.
/// Tries to parse as a slot commit event, falls back to any JSON with a "slot" field.
fn read_last_slot_from_topic(
    config: &KafkaSinkConfig,
    topic: &str,
) -> Result<Option<LastCommitted>, String> {
    config
        .validate_credentials()
        .map_err(|e| format!("Invalid Kafka sink config: {e}"))?;

    let mut client_config = ClientConfig::new();

    client_config
        .set("bootstrap.servers", &config.brokers)
        .set("group.id", "shipstern-startup-reader")
        .set("enable.auto.commit", "false")
        .set("isolation.level", "read_committed"); // read only committed kafka transactions

    config.apply_sasl_if_configured(&mut client_config);

    let consumer: BaseConsumer = match client_config.create() {
        Ok(consumer) => consumer,
        Err(e) => {
            return Err(format!(
                "Failed to create startup Kafka consumer for topic {topic}: {e}"
            ));
        },
    };

    let metadata = match consumer.fetch_metadata(Some(topic), Duration::from_secs(5)) {
        Ok(m) => m,
        Err(e) => return Err(format!("Failed to fetch metadata for topic {topic}: {e}")),
    };

    let Some(topic_metadata) = metadata.topics().iter().find(|t| t.name() == topic) else {
        return Ok(None);
    };

    if topic_metadata.partitions().is_empty() {
        tracing::info!(topic, "Topic has no partitions — starting fresh");

        return Ok(None);
    }

    let mut latest: Option<LastCommitted> = None;

    for partition in topic_metadata.partitions() {
        let partition_id = partition.id();

        let (low, high) = consumer
            .fetch_watermarks(topic, partition_id, Duration::from_secs(5))
            .map_err(|e| {
                format!(
                    "Failed to fetch watermarks for topic {topic} partition {partition_id}: {e}"
                )
            })?;

        if high == 0 {
            continue;
        }

        let scan_low = startup_scan_low_offset(low, high);

        let candidate =
            read_tail_candidate_in_range(&consumer, topic, partition_id, scan_low, high)?;

        if let Some(candidate) = candidate {
            tracing::debug!(
                topic,
                partition = partition_id,
                kafka_offset = candidate.offset,
                slot = candidate.slot,
                "Selected startup checkpoint candidate"
            );
        }

        if let Some(candidate) = candidate {
            if latest.is_none_or(|current| candidate.slot > current.slot) {
                latest = Some(LastCommitted {
                    slot: candidate.slot,
                });
            }
        } else if high > low {
            let scanned_offsets = high - scan_low;

            let reason = if scan_low > low {
                "scan limit reached before finding a checkpoint"
            } else {
                "no parseable checkpoint record found"
            };

            return Err(format!(
                "Failed to find committed checkpoint in topic {topic} partition {partition_id} \
                 after scanning {scanned_offsets} tail offsets (low={low}, high={high}); {reason}"
            ));
        }
    }

    Ok(latest)
}

fn startup_scan_low_offset(low: i64, high: i64) -> i64 {
    (high - MAX_STARTUP_CHECKPOINT_SCAN_OFFSETS).max(low)
}

fn read_tail_candidate_in_range(
    consumer: &BaseConsumer,
    topic: &str,
    partition_id: i32,
    scan_low: i64,
    scan_high: i64,
) -> Result<Option<LastCommittedCandidate>, String> {
    let mut tpl = TopicPartitionList::new();

    tpl.add_partition_offset(topic, partition_id, rdkafka::Offset::Offset(scan_low))
        .map_err(|e| {
            format!(
                "Failed to prepare startup checkpoint assignment for topic {topic} partition \
                 {partition_id} offset {scan_low}: {e}"
            )
        })?;

    consumer.assign(&tpl).map_err(|e| {
        format!(
            "Failed to assign startup checkpoint consumer for topic {topic} partition \
             {partition_id} offset {scan_low}: {e}"
        )
    })?;

    consumer
        .seek(
            topic,
            partition_id,
            rdkafka::Offset::Offset(scan_low),
            Duration::from_secs(5),
        )
        .map_err(|e| {
            format!(
                "Failed to seek startup checkpoint consumer for topic {topic} partition \
                 {partition_id} offset {scan_low}: {e}"
            )
        })?;

    let deadline = Instant::now() + STARTUP_CHECKPOINT_SCAN_DEADLINE;
    let mut latest: Option<LastCommittedCandidate> = None;

    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(latest);
        };

        let message = match consumer.poll(remaining.min(Duration::from_millis(50))) {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                tracing::debug!(
                    topic,
                    partition = partition_id,
                    scan_low,
                    scan_high,
                    ?error,
                    "Ignoring Kafka poll error while scanning startup checkpoint"
                );
                continue;
            },
            None => continue,
        };

        if message.partition() != partition_id || message.offset() < scan_low {
            continue;
        }

        if message.offset() >= scan_high {
            return Ok(latest);
        }

        let Some(payload) = message.payload() else {
            continue;
        };

        if let Some(candidate) = parse_last_committed_candidate(message.offset(), payload) {
            latest = Some(candidate);
        }
    }
}

fn parse_last_committed_payload(payload: &[u8]) -> Option<LastCommitted> {
    serde_json::from_slice::<TransactionSlotCommitEvent>(payload)
        .ok()
        .map(|e| LastCommitted { slot: e.slot })
        .or_else(|| {
            serde_json::from_slice::<serde_json::Value>(payload)
                .ok()
                .and_then(|v| v.get("slot")?.as_u64())
                .map(|slot| LastCommitted { slot })
        })
}

fn parse_last_committed_candidate(
    message_offset: i64,
    payload: &[u8],
) -> Option<LastCommittedCandidate> {
    parse_last_committed_payload(payload).map(|commit| LastCommittedCandidate {
        slot: commit.slot,
        offset: message_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_last_committed_payload_reads_transaction_commit_event() {
        let payload = br#"{"slot":42,"blockhash":"abc","transaction_count":1,"decoded_instruction_count":1,"decode_filtered_instruction_count":0,"decode_error_instruction_count":0,"fallback_instruction_count":0,"transaction_status_failed_count":0,"transaction_status_succeeded_count":1}"#;

        let parsed = parse_last_committed_payload(payload);

        assert!(matches!(parsed, Some(LastCommitted { slot: 42 })));
    }

    #[test]
    fn parse_last_committed_payload_falls_back_to_generic_slot_json() {
        let payload = br#"{"slot":99,"marker_type":"Watermark"}"#;

        let parsed = parse_last_committed_payload(payload);

        assert!(matches!(parsed, Some(LastCommitted { slot: 99 })));
    }

    #[test]
    fn parse_last_committed_candidate_preserves_message_offset() {
        let payload = br#"{"slot":101}"#;

        let parsed = parse_last_committed_candidate(42, payload);

        assert_eq!(
            parsed,
            Some(LastCommittedCandidate {
                slot: 101,
                offset: 42,
            })
        );
    }

    #[test]
    fn startup_scan_low_offset_uses_tail_bound_when_range_is_large() {
        assert_eq!(startup_scan_low_offset(0, 1_000), 744);
    }

    #[test]
    fn startup_scan_low_offset_keeps_low_watermark_when_range_is_small() {
        assert_eq!(startup_scan_low_offset(100, 150), 100);
    }

    #[test]
    fn read_last_committed_block_rejects_half_set_sasl_credentials() {
        let config = KafkaSinkConfig {
            brokers: "127.0.0.1:1".to_string(),
            sasl_password: Some("pass".to_string()),
            ..Default::default()
        };

        let err = read_last_committed_transaction_block(&config).unwrap_err();

        assert!(err.contains("KAFKA_SASL_USERNAME is missing"), "{err}");
    }
}
