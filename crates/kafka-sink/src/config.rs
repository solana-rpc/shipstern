use std::{fmt, io, str::FromStr};

use serde::{Deserialize, Serialize};
use shipstern_block_coordinator::AccountMode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KafkaCompressionType {
    None,
    Gzip,
    Snappy,
    Lz4,
    // Zstd is a modern compression algorithm that offers better compression ratios
    // and faster speeds than older algorithms like Gzip and Snappy.
    #[default]
    Zstd,
}

impl KafkaCompressionType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Gzip => "gzip",
            Self::Snappy => "snappy",
            Self::Lz4 => "lz4",
            Self::Zstd => "zstd",
        }
    }
}

impl fmt::Display for KafkaCompressionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}

impl FromStr for KafkaCompressionType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "gzip" => Ok(Self::Gzip),
            "snappy" => Ok(Self::Snappy),
            "lz4" => Ok(Self::Lz4),
            "zstd" => Ok(Self::Zstd),
            other => Err(format!(
                "Invalid KAFKA_COMPRESSION_TYPE='{other}'. Expected one of: none, gzip, snappy, \
                 lz4, zstd"
            )),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KafkaSinkConfig {
    /// Kafka bootstrap servers (e.g., "localhost:9092").
    pub brokers: String,

    /// Schema Registry URL (e.g., "http://localhost:8081").
    #[serde(default = "default_schema_registry_url")]
    pub schema_registry_url: String,

    /// Topic for instruction/transaction slot commit markers.
    #[serde(default = "default_transaction_slots_topic")]
    pub transaction_slots_topic: String,

    /// Topic for account slot commit markers.
    #[serde(default = "default_account_slots_topic")]
    pub account_slots_topic: String,

    /// How accounts are processed and output.
    #[serde(default)]
    pub account_mode: AccountMode,

    /// Buffer size for the processing channel and deduplication tracking.
    /// Used for mpsc channel capacity (handle bursts) and to prune old processed heights.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,

    #[serde(default = "default_message_timeout_ms")]
    pub message_timeout_ms: u32,

    #[serde(default = "default_queue_buffering_max_messages")]
    pub queue_buffering_max_messages: u32,

    #[serde(default = "default_batch_num_messages")]
    pub batch_num_messages: u32,

    #[serde(default)]
    pub compression_type: KafkaCompressionType,

    /// Concrete Kafka transactional.id for this producer instance.
    ///
    /// When set, buffered sinks can atomically commit all records for a slot and
    /// the slot checkpoint marker in a single Kafka transaction.
    #[serde(default)]
    pub transactional_id: Option<String>,

    /// Broker-side transaction timeout used for transactional producers.
    #[serde(default = "default_transaction_timeout_ms")]
    pub transaction_timeout_ms: u32,

    /// Max attempts for Kafka writes before surfacing an error.
    #[serde(default = "default_kafka_write_max_attempts")]
    pub kafka_write_max_attempts: u32,

    /// Max attempts for Kafka transactional control operations such as
    /// init/begin/commit/abort before surfacing an error.
    #[serde(default = "default_kafka_transaction_op_max_attempts")]
    pub kafka_transaction_op_max_attempts: u32,

    /// Delay between Kafka write retry attempts.
    #[serde(default = "default_kafka_retry_backoff_ms")]
    pub kafka_retry_backoff_ms: u64,

    /// SASL username (e.g. "janus-dev"). When set, enables SASL_SSL + SCRAM-SHA-256.
    #[serde(default)]
    pub sasl_username: Option<String>,

    /// SASL password. Redacted from Debug and Serialize output.
    #[serde(default, skip_serializing)]
    pub sasl_password: Option<String>,

    /// Schema Registry username. Falls back to sasl_username when unset.
    #[serde(default)]
    pub schema_registry_username: Option<String>,

    /// Schema Registry password. Falls back to sasl_password when unset.
    /// Redacted from Debug and Serialize output.
    #[serde(default, skip_serializing)]
    pub schema_registry_password: Option<String>,
}

impl fmt::Debug for KafkaSinkConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KafkaSinkConfig")
            .field("brokers", &self.brokers)
            .field("schema_registry_url", &self.schema_registry_url)
            .field("transaction_slots_topic", &self.transaction_slots_topic)
            .field("account_slots_topic", &self.account_slots_topic)
            .field("account_mode", &self.account_mode)
            .field("buffer_size", &self.buffer_size)
            .field("message_timeout_ms", &self.message_timeout_ms)
            .field(
                "queue_buffering_max_messages",
                &self.queue_buffering_max_messages,
            )
            .field("batch_num_messages", &self.batch_num_messages)
            .field("producer_compression_type", &self.compression_type.as_str())
            .field("transactional_id", &self.transactional_id)
            .field("transaction_timeout_ms", &self.transaction_timeout_ms)
            .field("kafka_write_max_attempts", &self.kafka_write_max_attempts)
            .field(
                "kafka_transaction_op_max_attempts",
                &self.kafka_transaction_op_max_attempts,
            )
            .field("kafka_retry_backoff_ms", &self.kafka_retry_backoff_ms)
            .field("sasl_username", &self.sasl_username)
            .field(
                "sasl_password",
                &self.sasl_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("schema_registry_username", &self.schema_registry_username)
            .field(
                "schema_registry_password",
                &self.schema_registry_password.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

fn default_schema_registry_url() -> String { "http://localhost:8081".to_string() }

fn default_transaction_slots_topic() -> String { "transaction.slots".to_string() }

fn default_account_slots_topic() -> String { "account.slots".to_string() }

fn default_buffer_size() -> usize { 100 }

fn default_message_timeout_ms() -> u32 { 5000 }

fn default_queue_buffering_max_messages() -> u32 { 100000 }

fn default_batch_num_messages() -> u32 { 1000 }

fn default_transaction_timeout_ms() -> u32 { 30000 }

fn default_kafka_write_max_attempts() -> u32 { 3 }

fn default_kafka_transaction_op_max_attempts() -> u32 { 2 }

fn default_kafka_retry_backoff_ms() -> u64 { 200 }

impl Default for KafkaSinkConfig {
    fn default() -> Self {
        Self {
            brokers: "localhost:9092".to_string(),
            schema_registry_url: default_schema_registry_url(),
            transaction_slots_topic: default_transaction_slots_topic(),
            account_slots_topic: default_account_slots_topic(),
            account_mode: AccountMode::default(),
            buffer_size: default_buffer_size(),
            message_timeout_ms: default_message_timeout_ms(),
            queue_buffering_max_messages: default_queue_buffering_max_messages(),
            batch_num_messages: default_batch_num_messages(),
            compression_type: KafkaCompressionType::default(),
            transactional_id: None,
            transaction_timeout_ms: default_transaction_timeout_ms(),
            kafka_write_max_attempts: default_kafka_write_max_attempts(),
            kafka_transaction_op_max_attempts: default_kafka_transaction_op_max_attempts(),
            kafka_retry_backoff_ms: default_kafka_retry_backoff_ms(),
            sasl_username: None,
            sasl_password: None,
            schema_registry_username: None,
            schema_registry_password: None,
        }
    }
}

impl KafkaSinkConfig {
    pub fn new(brokers: impl Into<String>, schema_registry_url: impl Into<String>) -> Self {
        Self {
            brokers: brokers.into(),
            schema_registry_url: schema_registry_url.into(),
            ..Default::default()
        }
    }

    /// Validate credential pairs: each pair must be "both set or both unset".
    /// Call this at startup before creating any Kafka client.
    pub fn validate_credentials(&self) -> io::Result<()> {
        if let Some(id) = &self.transactional_id
            && id.trim().is_empty()
        {
            return Err(io::Error::other(
                "transactional_id must not be empty when transactions are enabled",
            ));
        }

        match (&self.sasl_username, &self.sasl_password) {
            (Some(_), None) => {
                return Err(io::Error::other(
                    "KAFKA_SASL_USERNAME is set but KAFKA_SASL_PASSWORD is missing",
                ));
            },
            (None, Some(_)) => {
                return Err(io::Error::other(
                    "KAFKA_SASL_PASSWORD is set but KAFKA_SASL_USERNAME is missing",
                ));
            },
            _ => {},
        }

        match (
            &self.schema_registry_username,
            &self.schema_registry_password,
        ) {
            (Some(_), None) => {
                return Err(io::Error::other(
                    "SCHEMA_REGISTRY_USERNAME is set but SCHEMA_REGISTRY_PASSWORD is missing",
                ));
            },
            (None, Some(_)) => {
                return Err(io::Error::other(
                    "SCHEMA_REGISTRY_PASSWORD is set but SCHEMA_REGISTRY_USERNAME is missing",
                ));
            },
            _ => {},
        }

        Ok(())
    }

    /// Apply SASL+TLS settings to an rdkafka ClientConfig when credentials are present.
    pub fn apply_sasl_if_configured(&self, client_config: &mut rdkafka::ClientConfig) {
        if let (Some(username), Some(password)) = (&self.sasl_username, &self.sasl_password) {
            client_config
                .set("security.protocol", "SASL_SSL")
                .set("sasl.mechanism", "SCRAM-SHA-256")
                .set("sasl.username", username)
                .set("sasl.password", password);
        }
    }

    /// Apply Basic Auth to a reqwest RequestBuilder for Schema Registry.
    ///
    /// Uses dedicated schema_registry pair if both set, otherwise falls back to
    /// Kafka SASL pair. After validate_credentials(), each pair is guaranteed to
    /// be "both or neither".
    pub fn apply_schema_registry_auth_if_configured(
        &self,
        req: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        let creds = match (
            &self.schema_registry_username,
            &self.schema_registry_password,
        ) {
            (Some(u), Some(p)) => Some((u, p)),
            _ => match (&self.sasl_username, &self.sasl_password) {
                (Some(u), Some(p)) => Some((u, p)),
                _ => None,
            },
        };

        match creds {
            Some((u, p)) => req.basic_auth(u, Some(p)),
            None => req,
        }
    }
}

#[cfg(test)]
mod tests {
    use shipstern_block_coordinator::AccountCommitAt;

    use super::*;

    #[test]
    fn account_mode_defaults_to_finalized_passthrough() {
        let config = KafkaSinkConfig::default();

        assert!(matches!(
            config.account_mode,
            AccountMode::FinalizedPassthrough
        ));
    }

    #[test]
    fn account_commit_at_defaults_to_confirmed() {
        let commit_at = AccountCommitAt::default();

        assert_eq!(commit_at, AccountCommitAt::Confirmed);
    }

    #[test]
    fn processed_finalized_round_trips_through_serde() {
        let mode = AccountMode::Processed {
            commit_at: AccountCommitAt::Finalized,
        };

        let json = serde_json::to_string(&mode).unwrap();

        let deserialized: AccountMode = serde_json::from_str(&json).unwrap();

        match deserialized {
            AccountMode::Processed { commit_at } => {
                assert_eq!(commit_at, AccountCommitAt::Finalized);
            },
            _ => panic!("Expected Processed, got: {deserialized:?}"),
        }
    }

    #[test]
    fn slots_topic_defaults() {
        let config = KafkaSinkConfig::default();

        assert_eq!(config.transaction_slots_topic, "transaction.slots");
        assert_eq!(config.account_slots_topic, "account.slots");
    }

    #[test]
    fn kafka_retry_defaults() {
        let config = KafkaSinkConfig::default();

        assert_eq!(config.kafka_write_max_attempts, 3);
        assert_eq!(config.kafka_transaction_op_max_attempts, 2);
        assert_eq!(config.kafka_retry_backoff_ms, 200);
    }

    #[test]
    fn validate_credentials_requires_complete_pairs() {
        let with = |sasl: (Option<&str>, Option<&str>), sr: (Option<&str>, Option<&str>)| {
            KafkaSinkConfig {
                sasl_username: sasl.0.map(Into::into),
                sasl_password: sasl.1.map(Into::into),
                schema_registry_username: sr.0.map(Into::into),
                schema_registry_password: sr.1.map(Into::into),
                ..KafkaSinkConfig::default()
            }
        };
        let (user, pass, none) = (Some("user"), Some("pass"), None);

        for (config, missing) in [
            (
                with((user, none), (none, none)),
                "KAFKA_SASL_PASSWORD is missing",
            ),
            (
                with((none, pass), (none, none)),
                "KAFKA_SASL_USERNAME is missing",
            ),
            (
                with((none, none), (user, none)),
                "SCHEMA_REGISTRY_PASSWORD is missing",
            ),
            (
                with((none, none), (none, pass)),
                "SCHEMA_REGISTRY_USERNAME is missing",
            ),
        ] {
            let err = config.validate_credentials().unwrap_err();
            assert!(err.to_string().contains(missing), "{err}");
        }

        for config in [
            with((none, none), (none, none)),
            with((user, pass), (none, none)),
            with((user, pass), (user, pass)),
        ] {
            config.validate_credentials().unwrap();
        }
    }

    #[test]
    fn validate_credentials_rejects_empty_transactional_id() {
        let config = KafkaSinkConfig {
            transactional_id: Some("   ".into()),
            ..KafkaSinkConfig::default()
        };

        let err = config.validate_credentials().unwrap_err();
        assert!(err.to_string().contains("transactional_id"));
    }
}
