//! The System program nonce account. Codama gives both of its enums a `u32` tag.

use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/system_nonce.json");

/// An initialized nonce account, as `solana-nonce` serializes it with bincode.
fn initialized_nonce() -> Vec<u8> {
    let mut data = Vec::with_capacity(80);
    data.extend_from_slice(&1_u32.to_le_bytes()); // version: current
    data.extend_from_slice(&1_u32.to_le_bytes()); // state: initialized
    data.extend_from_slice(&[0xaa; 32]); // authority
    data.extend_from_slice(&[0xbb; 32]); // durable nonce
    data.extend_from_slice(&5000_u64.to_le_bytes()); // lamports per signature
    data
}

#[test]
fn nonce_account_enum_tags_use_the_codama_size() {
    let data = initialized_nonce();

    let parsed = system::SystemAccount::try_unpack(&data).expect("an 80-byte nonce account");
    let system::account::Account::Nonce(nonce) = parsed.account;

    assert!(matches!(
        nonce.version.kind,
        system::nonce_version::Kind::Current(_)
    ));
    assert!(matches!(
        nonce.state.kind,
        system::nonce_state::Kind::Initialized(_)
    ));
    assert_eq!(nonce.authority.0, [0xaa; 32]);
    assert_eq!(nonce.blockhash.0, [0xbb; 32]);
    assert_eq!(nonce.lamports_per_signature, 5000);
    assert_eq!(borsh::to_vec(&nonce).expect("encodes"), data);
}
