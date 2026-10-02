use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};
use shipstern_core::Parser;
use shipstern_mock::tx_fixture;
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::{check_protobuf_format, p};

include_shipstern_parser!("../idls/squads_multisig_program.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(squads_multisig_program::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(squads_multisig_program::PROTOBUF_SCHEMA);
}

///
/// Regression: a top-level `vaultTransactionCreateArgs` type collided with the
/// instruction's args wrapper, so the encoder nested one layer deeper than
/// `PROTOBUF_SCHEMA`. A real instruction round-trips through `prost-reflect`.
///
#[tokio::test]
async fn schema_matches_encoder_for_vault_transaction_create() {
    let parser = squads_multisig_program::InstructionParser;

    let ixs = tx_fixture!(
        "1JXEeNsaTdwwnTAsQBLvA63cMSh3uKHnCLB8cB23oHdcszMM82k3skn79RmFfi9iUvav1spsUCG3C5QXT6U9Du2",
        &parser
    );

    let instructions = ixs
        .iter()
        .find_map(|ix| ix.as_ref())
        .expect("no instructions in fixture");

    let (vtc_accounts, vtc_args) = match &instructions.instruction {
        squads_multisig_program::instruction::Instruction::VaultTransactionCreate {
            accounts,
            args,
        } => (accounts, args),
        other => panic!("expected VaultTransactionCreate, got {other:?}"),
    };

    let expected = squads_multisig_program::instruction::VaultTransactionCreate {
        accounts: squads_multisig_program::instruction::VaultTransactionCreateAccounts {
            multisig: p("2ZDFsDzcztw6fRgpvRECytSDTnWtJbUDiVqck7CJS1oi"),
            transaction: p("Hofo61V8nGVimkARLFfozd9SuXRKsmptnJZbYZFMc9sk"),
            creator: p("cc3novbXuNSe292qKH2gGhxToaWjuBvJbA7zQf8NVxi"),
            rent_payer: p("cc3novbXuNSe292qKH2gGhxToaWjuBvJbA7zQf8NVxi"),
            system_program: p("11111111111111111111111111111111"),
            remaining_accounts: vec![],
        },
        args: squads_multisig_program::VaultTransactionCreateArgs {
            vault_index: 0,
            ephemeral_signers: 0,
            transaction_message: hex::decode(
                "01010109b849bd27f5a5600e89cfa703e6b186ba83a8713bf0684363ec7e3246d533fcf2\
                 f0aab114c69d47cbd0b2e31c5cdc6cda7cecfb29321e3bb59e669b0ee1e3bb000b7065b1\
                 e3d17c45389d527f6b04c3cd58b86c731aa0fdb549b6d1bc03f82946984c162de7c9c78e\
                 c9c58f7081bf6728fc6eedfe2b2d7bc9fe9d95180b1900922a0f53b9460ffb695f2c39b1\
                 5181b7b690749fa10e5b6e071737937569a6d80000000000000000000000000000000000\
                 0000000000000000000000000000000006a7d517187bd16635dad40455fdc2c0c124c68f\
                 215675a5dbbacb5f0800000008aff894c3fa67c0163a30f234a33d3467f03aa16417315e\
                 b4b81427bd9ba6ed09862285e3710a90d51d9e4702de9a9dd5e9fdc8ac81d2d2acd1e1dd\
                 c824fec401020b0002020301040005060708a700320000011f00000032303134202358592d\
                 5020506974636827732050696b616368752d486f6c6f09000000434f4c4c4543544f523f\
                 00000068747470733a2f2f617277656176652e6e65742f31665a484b473833367962394a\
                 5166654f352d334f47646a78696e476e6f316b4543693859684171443055c80001010000\
                 00b849bd27f5a5600e89cfa703e6b186ba83a8713bf0684363ec7e3246d533fcf2006400\
                 00000000000000",
            )
            .expect("transaction_message hex literal must be valid"),
            memo: None,
        },
    };

    assert_eq!(vtc_accounts, &expected.accounts);
    assert_eq!(vtc_args, &expected.args);

    // Encode through the producer-side prost impl (what the kafka sink emits).
    let mut buf = Vec::new();

    instructions.encode(&mut buf).expect("encode failed");

    assert!(!buf.is_empty());

    // Build a descriptor pool from PROTOBUF_SCHEMA (what consumers see) and
    // try to decode the wire bytes against the schema's `Instructions` type.
    let fdp = protox_parse::parse("schema.proto", squads_multisig_program::PROTOBUF_SCHEMA)
        .expect("schema parse failed");

    let mut pool = DescriptorPool::new();

    pool.add_file_descriptor_proto(fdp)
        .expect("descriptor add failed");

    let inst_desc = pool
        .get_message_by_name("squads_multisig_program.Instructions")
        .expect("Instructions message missing from schema");

    let dynamic = DynamicMessage::decode(inst_desc, buf.as_slice()).expect(
        "schema-driven decode failed — encoder produced bytes that do not match PROTOBUF_SCHEMA",
    );

    // Verify the decoded oneof actually picked vault_transaction_create
    // (tag 17 in `Instructions.instruction`).
    let oneof_field = dynamic
        .fields()
        .find(|(f, _)| f.name() == "vault_transaction_create")
        .expect("vault_transaction_create variant missing in decoded payload");

    assert_eq!(oneof_field.0.number(), 17);
}
