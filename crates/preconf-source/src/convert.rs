//! From the raw bytes the feed sends to the proto shape the runtime reads.

use solana_message::{Hash, VersionedMessage};
use solana_transaction::{
    simple_vote_transaction_checker::is_simple_vote_transaction_impl,
    versioned::VersionedTransaction,
};
use yellowstone_grpc_proto::solana::storage::confirmed_block as proto;

/// The bytes are the ones the block will carry, so trailing bytes are a bad message.
pub(crate) fn decode(bytes: &[u8]) -> Result<VersionedTransaction, wincode::ReadError> {
    wincode::deserialize_exact(bytes)
}

pub(crate) fn is_vote(tx: &VersionedTransaction) -> bool {
    let keys = tx.message.static_account_keys();
    let programs = tx
        .message
        .instructions()
        .iter()
        .filter_map(|ix| keys.get(usize::from(ix.program_id_index)));

    is_simple_vote_transaction_impl(
        &tx.signatures,
        matches!(tx.message, VersionedMessage::Legacy(_)),
        programs,
    )
}

/// The three message fields that depend on the version. Every match arm in
/// [`transaction`] states all three instead of inheriting a default.
struct MessageParts {
    header: solana_message::MessageHeader,
    account_keys: Vec<solana_pubkey::Pubkey>,
    /// Legacy and V0 call this `recent_blockhash`, V1 calls it
    /// `lifetime_specifier`. The proto carries them in the same field.
    lifetime: Hash,
    instructions: Vec<solana_message::compiled_instruction::CompiledInstruction>,
    versioned: bool,
    address_table_lookups: Vec<proto::MessageAddressTableLookup>,
    config: Option<proto::TransactionConfig>,
}

pub(crate) fn transaction(tx: VersionedTransaction) -> proto::Transaction {
    let signatures = tx.signatures.iter().map(|s| s.as_ref().to_vec()).collect();

    let parts = match tx.message {
        VersionedMessage::Legacy(msg) => MessageParts {
            header: msg.header,
            account_keys: msg.account_keys,
            lifetime: msg.recent_blockhash,
            instructions: msg.instructions,
            versioned: false,
            address_table_lookups: vec![],
            config: None,
        },
        VersionedMessage::V0(msg) => MessageParts {
            header: msg.header,
            account_keys: msg.account_keys,
            lifetime: msg.recent_blockhash,
            instructions: msg.instructions,
            versioned: true,
            address_table_lookups: msg
                .address_table_lookups
                .into_iter()
                .map(|l| proto::MessageAddressTableLookup {
                    account_key: l.account_key.as_ref().to_vec(),
                    writable_indexes: l.writable_indexes,
                    readonly_indexes: l.readonly_indexes,
                })
                .collect(),
            config: None,
        },
        // The proto marks V1 by the presence of `config`, so it is always `Some`.
        VersionedMessage::V1(msg) => MessageParts {
            header: msg.header,
            account_keys: msg.account_keys,
            lifetime: msg.lifetime_specifier,
            instructions: msg.instructions,
            versioned: true,
            address_table_lookups: vec![],
            config: Some(proto::TransactionConfig {
                priority_fee: msg.config.priority_fee,
                compute_unit_limit: msg.config.compute_unit_limit,
                loaded_accounts_data_size_limit: msg.config.loaded_accounts_data_size_limit,
                heap_size: msg.config.heap_size,
            }),
        },
    };

    let message = proto::Message {
        header: Some(proto::MessageHeader {
            num_required_signatures: u32::from(parts.header.num_required_signatures),
            num_readonly_signed_accounts: u32::from(parts.header.num_readonly_signed_accounts),
            num_readonly_unsigned_accounts: u32::from(parts.header.num_readonly_unsigned_accounts),
        }),
        account_keys: parts
            .account_keys
            .iter()
            .map(|k| k.as_ref().to_vec())
            .collect(),
        recent_blockhash: parts.lifetime.to_bytes().to_vec(),
        instructions: parts
            .instructions
            .into_iter()
            .map(|ix| proto::CompiledInstruction {
                program_id_index: u32::from(ix.program_id_index),
                accounts: ix.accounts,
                data: ix.data,
            })
            .collect(),
        versioned: parts.versioned,
        address_table_lookups: parts.address_table_lookups,
        config: parts.config,
    };

    proto::Transaction {
        signatures,
        message: Some(message),
    }
}

/// The feed reports an outcome and nothing else, so every other field is absent.
/// A failure keeps `err` set with empty bytes: the cause is unknown, only
/// `is_some()` carries meaning.
pub(crate) fn meta(failed: bool) -> proto::TransactionStatusMeta {
    proto::TransactionStatusMeta {
        err: failed.then(|| proto::TransactionError { err: Vec::new() }),
        inner_instructions_none: true,
        log_messages_none: true,
        return_data_none: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use yellowstone_grpc_proto::solana::storage::confirmed_block as proto;

    use super::{decode, transaction};
    use crate::test_fixtures::{fixture, LEGACY, V0, V1};

    fn b58(bytes: &[u8]) -> String { bs58::encode(bytes).into_string() }

    fn u8s(value: &Value) -> Vec<u8> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
            .collect()
    }

    /// The fields every version shares, against the node's own parse of the same bytes.
    fn assert_message_matches(message: &proto::Message, json: &Value) {
        let header = message.header.as_ref().unwrap();
        assert_eq!(
            header.num_required_signatures,
            json["header"]["numRequiredSignatures"]
        );
        assert_eq!(
            header.num_readonly_signed_accounts,
            json["header"]["numReadonlySignedAccounts"]
        );
        assert_eq!(
            header.num_readonly_unsigned_accounts,
            json["header"]["numReadonlyUnsignedAccounts"]
        );

        let keys: Vec<String> = message.account_keys.iter().map(|k| b58(k)).collect();
        let json_keys: Vec<&str> = json["accountKeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert_eq!(keys, json_keys);
        assert_eq!(b58(&message.recent_blockhash), json["recentBlockhash"]);

        let json_ixs = json["instructions"].as_array().unwrap();
        assert_eq!(message.instructions.len(), json_ixs.len());
        for (ix, json_ix) in message.instructions.iter().zip(json_ixs) {
            assert_eq!(ix.program_id_index, json_ix["programIdIndex"]);
            assert_eq!(ix.accounts, u8s(&json_ix["accounts"]));
            assert_eq!(b58(&ix.data), json_ix["data"]);
        }
    }

    /// A legacy message maps field by field and is not marked versioned.
    #[test]
    fn legacy_message_matches_the_node_parse() {
        let (bytes, json) = fixture(LEGACY);
        let tx = decode(&bytes).unwrap();

        let converted = transaction(tx);

        let message = converted.message.as_ref().unwrap();
        assert_message_matches(message, &json["message"]);
        assert_eq!(converted.signatures.len(), 1);
        assert_eq!(b58(&converted.signatures[0]), LEGACY);
        assert!(!message.versioned);
        assert!(message.address_table_lookups.is_empty());
        assert!(message.config.is_none());
    }

    /// A v0 message keeps its lookup tables and is marked versioned.
    #[test]
    fn v0_message_keeps_its_lookup_tables() {
        let (bytes, json) = fixture(V0);
        let tx = decode(&bytes).unwrap();

        let converted = transaction(tx);

        let message = converted.message.as_ref().unwrap();
        assert_message_matches(message, &json["message"]);
        assert!(message.versioned);
        let json_lookups = json["message"]["addressTableLookups"].as_array().unwrap();
        assert_eq!(message.address_table_lookups.len(), json_lookups.len());
        for (lookup, json_lookup) in message.address_table_lookups.iter().zip(json_lookups) {
            assert_eq!(b58(&lookup.account_key), json_lookup["accountKey"]);
            assert_eq!(
                lookup.writable_indexes,
                u8s(&json_lookup["writableIndexes"])
            );
            assert_eq!(
                lookup.readonly_indexes,
                u8s(&json_lookup["readonlyIndexes"])
            );
        }
        assert!(message.config.is_none());
    }

    /// A v1 message carries its inline config, which marks it as v1 on the wire.
    #[test]
    fn v1_message_sets_its_config() {
        let (bytes, json) = fixture(V1);
        let tx = decode(&bytes).unwrap();

        let converted = transaction(tx);

        let message = converted.message.as_ref().unwrap();
        assert_message_matches(message, &json["message"]);
        assert!(message.versioned);
        let config = message.config.as_ref().unwrap();
        let json_config = &json["message"]["transactionConfig"];
        let u32_of = |value: &Value| value.as_u64().map(|v| u32::try_from(v).unwrap());
        assert_eq!(config.priority_fee, json_config["priorityFee"].as_u64());
        assert_eq!(
            config.compute_unit_limit,
            u32_of(&json_config["computeUnitLimit"])
        );
        assert_eq!(
            config.loaded_accounts_data_size_limit,
            u32_of(&json_config["loadedAccountsDataSizeLimit"])
        );
        assert_eq!(config.heap_size, u32_of(&json_config["heapSize"]));
    }

    /// Bytes after the transaction are an error: the feed sends exactly the block's bytes.
    #[test]
    fn trailing_bytes_are_an_error() {
        let (mut bytes, _) = fixture(LEGACY);
        bytes.push(0xaa);

        assert!(decode(&bytes).is_err());
    }
}
