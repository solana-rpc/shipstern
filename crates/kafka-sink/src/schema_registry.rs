//! Schema Registry client for registering Protobuf schemas.
//!
//! Uses the Confluent Schema Registry REST API (compatible with Redpanda).

use std::{collections::HashMap, io};

use serde::{Deserialize, Serialize};

use crate::config::KafkaSinkConfig;

pub struct SchemaDefinition {
    /// Subject name (typically `"<topic>-value"` or `"<topic>-key"`).
    pub subject: String,
    /// The protobuf schema content.
    pub schema: &'static str,
    /// Index of the message type within the schema (0-indexed from the last message).
    /// For a schema with only one message, this should be 0.
    /// For our TokenProgramInstruction which is the last message, this is 0.
    pub message_index: i32,
}

#[derive(Clone, Debug)]
pub struct RegisteredSchema {
    pub schema_id: i32,
    pub message_index: i32,
}

const MAGIC_BYTE: u8 = 0x00;
const SCHEMA_ID_SIZE: usize = std::mem::size_of::<i32>();
const MAX_VARINT_SIZE: usize = 10;

// TODO: maybe can be optimized to not waste bytes
fn wire_format_max_capacity(indices_count: usize, payload_len: usize) -> usize {
    let max_indices_bytes = (1 + indices_count) * MAX_VARINT_SIZE;

    1 + SCHEMA_ID_SIZE + max_indices_bytes + payload_len
}

/// Format: `[0x00][4-byte schema ID BE][zigzag varint message indices...][protobuf payload]`
/// See: <https://docs.confluent.io/platform/current/schema-registry/fundamentals/serdes-develop/index.html>
pub fn wrap_payload_with_confluent_wire_format(
    schema_id: i32,
    message_indices: &[i32],
    payload: &[u8],
) -> Vec<u8> {
    let capacity = wire_format_max_capacity(message_indices.len(), payload.len());

    let mut buf = Vec::with_capacity(capacity);

    buf.push(MAGIC_BYTE);
    buf.extend_from_slice(&schema_id.to_be_bytes());
    buf.extend(encode_indices(message_indices));
    buf.extend_from_slice(payload);

    buf
}

fn encode_indices(indices: &[i32]) -> impl Iterator<Item = u8> + '_ {
    encode_zigzag(indices.len() as i64).chain(indices.iter().flat_map(|&i| encode_zigzag(i as i64)))
}

/// Encode a value as zig-zag varint bytes (lazy iterator, no allocation).
/// Zig-zag encoding: (n << 1) ^ (n >> 63) maps signed to unsigned for efficient varint.
fn encode_zigzag(value: i64) -> ZigzagIter {
    let zigzag = ((value << 1) ^ (value >> 63)) as u64;

    ZigzagIter {
        value: zigzag,
        done: false,
    }
}

/// Iterator that yields zig-zag encoded varint bytes lazily.
struct ZigzagIter {
    value: u64,
    done: bool,
}

impl Iterator for ZigzagIter {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.done {
            return None;
        }

        let mut byte = (self.value & 0x7f) as u8;

        self.value >>= 7;

        if self.value != 0 {
            byte |= 0x80;
        } else {
            self.done = true;
        }

        Some(byte)
    }
}

#[derive(Serialize)]
struct SchemaReference {
    name: String,
    subject: String,
    version: i32,
}

#[derive(Serialize)]
struct RegisterSchemaRequest<'a> {
    #[serde(rename = "schemaType")]
    schema_type: &'a str,
    schema: &'a str,
    references: Vec<SchemaReference>,
}

#[derive(Deserialize)]
struct RegisterSchemaResponse {
    id: i32,
}

#[derive(Deserialize)]
struct SchemaRegistryError {
    error_code: i32,
    message: String,
}

/// Register a protobuf schema with the Schema Registry.
/// Returns the schema ID on success.
fn register_schema(
    client: &reqwest::blocking::Client,
    base_url: &str,
    subject: &str,
    schema: &str,
    config: &KafkaSinkConfig,
) -> Result<i32, String> {
    let url = format!("{}/subjects/{}/versions", base_url, subject);

    let request = RegisterSchemaRequest {
        schema_type: "PROTOBUF",
        schema,
        references: vec![],
    };

    let req = client
        .post(&url)
        .header("Content-Type", "application/vnd.schemaregistry.v1+json")
        .json(&request);

    let req = config.apply_schema_registry_auth_if_configured(req);

    let response = req
        .send()
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    let status = response.status();

    if status.is_success() {
        let result: RegisterSchemaResponse = response
            .json()
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        Ok(result.id)
    } else if status.as_u16() == 409 {
        // Schema already exists or is incompatible
        let error: SchemaRegistryError = response
            .json()
            .map_err(|e| format!("Failed to parse error response: {}", e))?;

        Err(format!(
            "Schema conflict (code {}): {}",
            error.error_code, error.message
        ))
    } else {
        let error_text = response
            .text()
            .unwrap_or_else(|_| "Unknown error".to_string());

        Err(format!("HTTP {}: {}", status, error_text))
    }
}

fn check_schema_registry(
    client: &reqwest::blocking::Client,
    base_url: &str,
    config: &KafkaSinkConfig,
) -> io::Result<()> {
    let req = client
        .get(format!("{}/subjects", base_url))
        .timeout(std::time::Duration::from_secs(5));

    let req = config.apply_schema_registry_auth_if_configured(req);

    match req.send() {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) if r.status().as_u16() == 401 || r.status().as_u16() == 403 => Err(io::Error::other(
            format!("Schema Registry auth failed (HTTP {})", r.status()),
        )),
        Ok(r) => Err(io::Error::other(format!(
            "Schema Registry returned HTTP {}",
            r.status()
        ))),
        Err(e) => Err(io::Error::other(format!(
            "Schema Registry unreachable: {e}"
        ))),
    }
}

/// Register all provided schemas with the Schema Registry.
/// Returns a map of subject -> RegisteredSchema for encoding messages.
pub fn ensure_schemas_registered(
    config: &KafkaSinkConfig,
    schemas: &[SchemaDefinition],
) -> io::Result<HashMap<String, RegisteredSchema>> {
    let base_url = &config.schema_registry_url;
    let mut registered = HashMap::new();

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    check_schema_registry(&client, base_url, config)?;

    tracing::info!(url = %base_url, "Registering schemas with Schema Registry");

    for schema_def in schemas {
        let schema_id = register_schema(
            &client,
            base_url,
            &schema_def.subject,
            schema_def.schema,
            config,
        )
        .map_err(|e| {
            io::Error::other(format!(
                "Failed to register schema for {}: {e}",
                schema_def.subject
            ))
        })?;

        tracing::debug!(subject = %schema_def.subject, schema_id, "Schema ready");

        registered.insert(schema_def.subject.clone(), RegisteredSchema {
            schema_id,
            message_index: schema_def.message_index,
        });
    }

    Ok(registered)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_schema_does_not_use_the_latest_schema_id() {
        let mut server = mockito::Server::new();
        let subject = "example-value";

        server
            .mock("GET", "/subjects")
            .with_status(200)
            .with_body("[]")
            .create();
        server
            .mock("POST", "/subjects/example-value/versions")
            .with_status(409)
            .with_body(r#"{"error_code": 40901, "message": "Incompatible schema"}"#)
            .create();
        server
            .mock("GET", "/subjects/example-value/versions/latest")
            .with_status(200)
            .with_body(r#"{"id": 42}"#)
            .create();

        let config = KafkaSinkConfig {
            schema_registry_url: server.url(),
            ..Default::default()
        };
        let schemas = [SchemaDefinition {
            subject: subject.to_string(),
            schema: "syntax = \"proto3\"; message Example {}",
            message_index: 0,
        }];

        let error = ensure_schemas_registered(&config, &schemas).unwrap_err();

        assert!(error.to_string().contains("Schema conflict (code 40901)"));
    }

    #[test]
    fn schema_registry_auth_sends_basic_auth_header() {
        let mut server = mockito::Server::new();

        let mock = server
            .mock("GET", "/subjects")
            .match_header("authorization", "Basic dXNlcjpwYXNz") // base64("user:pass")
            .with_status(200)
            .with_body("[]")
            .create();

        let config = KafkaSinkConfig {
            schema_registry_url: server.url(),
            sasl_username: Some("user".into()),
            sasl_password: Some("pass".into()),
            ..Default::default()
        };

        let client = reqwest::blocking::Client::new();

        let result = check_schema_registry(&client, &server.url(), &config);

        assert!(result.is_ok());

        mock.assert();
    }

    #[test]
    fn schema_registry_auth_falls_back_to_sasl_creds() {
        let mut server = mockito::Server::new();

        let mock = server
            .mock("GET", "/subjects")
            .match_header("authorization", "Basic dXNlcjpwYXNz")
            .with_status(200)
            .with_body("[]")
            .create();

        let config = KafkaSinkConfig {
            schema_registry_url: server.url(),
            sasl_username: Some("user".into()),
            sasl_password: Some("pass".into()),
            schema_registry_username: None,
            schema_registry_password: None,
            ..Default::default()
        };

        let client = reqwest::blocking::Client::new();

        let result = check_schema_registry(&client, &server.url(), &config);

        assert!(result.is_ok());

        mock.assert();
    }

    #[test]
    fn schema_registry_no_auth_when_no_creds() {
        let mut server = mockito::Server::new();

        let mock = server
            .mock("GET", "/subjects")
            .match_header("authorization", mockito::Matcher::Missing)
            .with_status(200)
            .with_body("[]")
            .create();

        let config = KafkaSinkConfig {
            schema_registry_url: server.url(),
            ..Default::default()
        };

        let client = reqwest::blocking::Client::new();

        let result = check_schema_registry(&client, &server.url(), &config);

        assert!(result.is_ok());

        mock.assert();
    }

    #[test]
    fn check_schema_registry_401_is_fatal() {
        let mut server = mockito::Server::new();

        server.mock("GET", "/subjects").with_status(401).create();

        let config = KafkaSinkConfig {
            schema_registry_url: server.url(),
            ..Default::default()
        };

        let client = reqwest::blocking::Client::new();

        let err = check_schema_registry(&client, &server.url(), &config).unwrap_err();

        assert!(err.to_string().contains("auth failed"));
    }

    #[test]
    fn check_schema_registry_403_is_fatal() {
        let mut server = mockito::Server::new();

        server.mock("GET", "/subjects").with_status(403).create();

        let config = KafkaSinkConfig {
            schema_registry_url: server.url(),
            ..Default::default()
        };

        let client = reqwest::blocking::Client::new();

        let err = check_schema_registry(&client, &server.url(), &config).unwrap_err();

        assert!(err.to_string().contains("auth failed"));
    }

    #[test]
    fn check_schema_registry_connection_error_is_fatal() {
        let config = KafkaSinkConfig {
            schema_registry_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .unwrap();

        let err = check_schema_registry(&client, &config.schema_registry_url, &config).unwrap_err();

        assert!(err.to_string().contains("unreachable"));
    }
}
