use shipstern_core::{Parser, Pubkey};
use shipstern_mock::tx_fixture;
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::{check_protobuf_format, p};

include_shipstern_parser!("../idls/pump_fun.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(pump_fun::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(pump_fun::PROTOBUF_SCHEMA);
}

#[tokio::test]
async fn parse_sell_ix() {
    let parser = pump_fun::InstructionParser;

    let ixs = tx_fixture!(
        "4GtxzHLTW8ZqhhHLqXPK2DSqtqaMXBvknpkXqgtfGjxaPvBC6AdHnzvX6X8EgMo9V4Ua4osroEpk7Q3f3oernYTQ",
        &parser
    );

    let (sell_accounts, sell_args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            pump_fun::instruction::Instruction::Sell { accounts, args } => Some((accounts, args)),
            _ => None,
        })
        .expect("no Sell found");

    let expected = pump_fun::instruction::Sell {
        accounts: pump_fun::instruction::SellAccounts {
            global: p("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf"),
            fee_recipient: p("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV"),
            mint: p("FXLjNRSQXuFM8fhKLEg2QeuUxs4Eu2hhc28tZhzrpump"),
            bonding_curve: p("9srUo8cgvHKkAZYNdjwgiYm8vK8PNRhBbzjh8RuWZtp3"),
            associated_bonding_curve: p("Ccpc35bJPjubzYbfkUbg7kK7Arkp4Q8so7ne6rLpAGXv"),
            associated_user: p("3K6Mgvd9jEvXRM7DTeNGjBZc2xi4QCqyg1mqVwaCWP76"),
            user: p("GEk94udrX63hAvNzNE1eC8zNvMDZ2cpV9xTgKuVP7kAw"),
            system_program: p("11111111111111111111111111111111"),
            creator_vault: p("7YbeWL1XuV6FCgYZbFSk7pcqk4UdM2G9YWuZKYHWmpfJ"),
            token_program: p("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
            event_authority: p("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1"),
            program: Pubkey::new(pump_fun::PROGRAM_ID),
            fee_config: p("8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt"),
            fee_program: p("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ"),
            remaining_accounts: vec![],
        },
        args: pump_fun::instruction::SellArgs {
            amount: 3_878_351_170_692,
            min_sol_output: 522_403_143,
        },
    };

    assert_eq!(sell_accounts, &expected.accounts);
    assert_eq!(sell_args, &expected.args);
}

#[tokio::test]
async fn parse_buy_ix() {
    let parser = pump_fun::InstructionParser;

    let ixs = tx_fixture!(
        "3tkxRjNDfth6NxXpYbbLKmPkPYyAD4jjXfNnDCYtCKSPN2zSpJXT29reowKtFKz1puY1fHmBFVAskkK2A7o8cZgJ",
        &parser
    );

    let (buy_accounts, buy_args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            pump_fun::instruction::Instruction::Buy { accounts, args } => Some((accounts, args)),
            _ => None,
        })
        .expect("no Buy found");

    let expected = pump_fun::instruction::Buy {
        accounts: pump_fun::instruction::BuyAccounts {
            global: p("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf"),
            fee_recipient: p("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV"),
            mint: p("BaWrqmzh9nqUkZRKB9p9WNaM8xKRrmcW9ztgLJrWpump"),
            bonding_curve: p("8DZu2LNtTKph9iShWpWdFjHPFAqGkrafpSUyaFZrd6ip"),
            associated_bonding_curve: p("FW755HTHAZweRrzoyZWe6iQdHwtvh82Z6HKcymWCUwCQ"),
            associated_user: p("GYNLNMVdaw8CtrgsiJhrwG5vmTXhei8Kc768FqBEEEsq"),
            user: p("DG6aWRd9ft47v9MZxeoHrxaJfVfLcXRTwC4RZ23DkokX"),
            system_program: p("11111111111111111111111111111111"),
            token_program: p("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
            creator_vault: p("BM11VKvie7n5CA6crAasQDeorhpvLJ3Ghu8BHkTpwRSq"),
            event_authority: p("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1"),
            program: Pubkey::new(pump_fun::PROGRAM_ID),
            global_volume_accumulator: p("Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y"),
            user_volume_accumulator: p("Guka6uenDqtwCT8xmGpirKWeD5KU3r5L21qCSBVPHHPY"),
            fee_config: p("8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt"),
            fee_program: p("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ"),
            remaining_accounts: vec![],
        },
        args: pump_fun::instruction::BuyArgs {
            amount: 693_868_985_905,
            max_sol_cost: 55_000_000,
            track_volume: pump_fun::OptionBool {},
        },
    };

    assert_eq!(buy_accounts, &expected.accounts);
    assert_eq!(buy_args, &expected.args);
}

#[test]
fn check_json_serialization() {
    // account
    let curve = pump_fun::BondingCurve::default();
    let json_str = serde_json::to_string(&curve).expect("failed to json serialize");
    let _: pump_fun::BondingCurve =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let buy = pump_fun::instruction::Buy::default();
    let json_str = serde_json::to_string(&buy).expect("failed to json serialize");
    let _: pump_fun::instruction::Buy =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}

#[test]
fn exposes_account_discriminators() {
    assert_eq!(
        pump_fun::BondingCurve::DISCRIMINATOR,
        hex::decode("17b7f83760d8ac60").unwrap().as_slice()
    );
    assert_eq!(pump_fun::BondingCurve::DISCRIMINATOR_OFFSET, 0);
}

///
/// Instruction variants are not types, so their discriminators hang on the
/// `Instructions` wrapper rather than on a per-variant impl.
///
#[test]
fn exposes_instruction_discriminators() {
    assert_eq!(
        pump_fun::Instructions::BUY_DISCRIMINATOR,
        hex::decode("66063d1201daebea").unwrap().as_slice()
    );
    assert_eq!(pump_fun::Instructions::BUY_DISCRIMINATOR_OFFSET, 0);

    assert_eq!(
        pump_fun::Instructions::ADMIN_SET_CREATOR_DISCRIMINATOR,
        hex::decode("4519ab8e39ef0d04").unwrap().as_slice()
    );
}

///
/// Every emitted instruction constant must be recognised by the matcher.
///
/// The constant is built by `extract_ix_discriminator_key` + `to_bytes_offset`,
/// while the parser matches through `extract_discriminator_info`, two separate
/// decode paths. Driving `resolve_instruction_default` with a buffer built from
/// each constant proves they agree: a mismatch surfaces as
/// `DiscriminatorNotFound`. Argument deserialization is expected to fail on
/// these synthetic buffers; only the discriminator verdict is asserted.
///
#[test]
fn every_instruction_const_is_recognised_by_the_matcher() {
    let all: &[(&[u8], usize, &str)] = &[
        (
            pump_fun::Instructions::ADMIN_SET_CREATOR_DISCRIMINATOR,
            pump_fun::Instructions::ADMIN_SET_CREATOR_DISCRIMINATOR_OFFSET,
            "ADMIN_SET_CREATOR",
        ),
        (
            pump_fun::Instructions::ADMIN_SET_IDL_AUTHORITY_DISCRIMINATOR,
            pump_fun::Instructions::ADMIN_SET_IDL_AUTHORITY_DISCRIMINATOR_OFFSET,
            "ADMIN_SET_IDL_AUTHORITY",
        ),
        (
            pump_fun::Instructions::ADMIN_UPDATE_TOKEN_INCENTIVES_DISCRIMINATOR,
            pump_fun::Instructions::ADMIN_UPDATE_TOKEN_INCENTIVES_DISCRIMINATOR_OFFSET,
            "ADMIN_UPDATE_TOKEN_INCENTIVES",
        ),
        (
            pump_fun::Instructions::BUY_DISCRIMINATOR,
            pump_fun::Instructions::BUY_DISCRIMINATOR_OFFSET,
            "BUY",
        ),
        (
            pump_fun::Instructions::BUY_EXACT_SOL_IN_DISCRIMINATOR,
            pump_fun::Instructions::BUY_EXACT_SOL_IN_DISCRIMINATOR_OFFSET,
            "BUY_EXACT_SOL_IN",
        ),
        (
            pump_fun::Instructions::CLAIM_CASHBACK_DISCRIMINATOR,
            pump_fun::Instructions::CLAIM_CASHBACK_DISCRIMINATOR_OFFSET,
            "CLAIM_CASHBACK",
        ),
        (
            pump_fun::Instructions::CLAIM_TOKEN_INCENTIVES_DISCRIMINATOR,
            pump_fun::Instructions::CLAIM_TOKEN_INCENTIVES_DISCRIMINATOR_OFFSET,
            "CLAIM_TOKEN_INCENTIVES",
        ),
        (
            pump_fun::Instructions::CLOSE_USER_VOLUME_ACCUMULATOR_DISCRIMINATOR,
            pump_fun::Instructions::CLOSE_USER_VOLUME_ACCUMULATOR_DISCRIMINATOR_OFFSET,
            "CLOSE_USER_VOLUME_ACCUMULATOR",
        ),
        (
            pump_fun::Instructions::COLLECT_CREATOR_FEE_DISCRIMINATOR,
            pump_fun::Instructions::COLLECT_CREATOR_FEE_DISCRIMINATOR_OFFSET,
            "COLLECT_CREATOR_FEE",
        ),
        (
            pump_fun::Instructions::CREATE_DISCRIMINATOR,
            pump_fun::Instructions::CREATE_DISCRIMINATOR_OFFSET,
            "CREATE",
        ),
        (
            pump_fun::Instructions::CREATE_V2_DISCRIMINATOR,
            pump_fun::Instructions::CREATE_V2_DISCRIMINATOR_OFFSET,
            "CREATE_V2",
        ),
        (
            pump_fun::Instructions::DISTRIBUTE_CREATOR_FEES_DISCRIMINATOR,
            pump_fun::Instructions::DISTRIBUTE_CREATOR_FEES_DISCRIMINATOR_OFFSET,
            "DISTRIBUTE_CREATOR_FEES",
        ),
        (
            pump_fun::Instructions::EXTEND_ACCOUNT_DISCRIMINATOR,
            pump_fun::Instructions::EXTEND_ACCOUNT_DISCRIMINATOR_OFFSET,
            "EXTEND_ACCOUNT",
        ),
        (
            pump_fun::Instructions::GET_MINIMUM_DISTRIBUTABLE_FEE_DISCRIMINATOR,
            pump_fun::Instructions::GET_MINIMUM_DISTRIBUTABLE_FEE_DISCRIMINATOR_OFFSET,
            "GET_MINIMUM_DISTRIBUTABLE_FEE",
        ),
        (
            pump_fun::Instructions::INIT_USER_VOLUME_ACCUMULATOR_DISCRIMINATOR,
            pump_fun::Instructions::INIT_USER_VOLUME_ACCUMULATOR_DISCRIMINATOR_OFFSET,
            "INIT_USER_VOLUME_ACCUMULATOR",
        ),
        (
            pump_fun::Instructions::INITIALIZE_DISCRIMINATOR,
            pump_fun::Instructions::INITIALIZE_DISCRIMINATOR_OFFSET,
            "INITIALIZE",
        ),
        (
            pump_fun::Instructions::MIGRATE_DISCRIMINATOR,
            pump_fun::Instructions::MIGRATE_DISCRIMINATOR_OFFSET,
            "MIGRATE",
        ),
        (
            pump_fun::Instructions::MIGRATE_BONDING_CURVE_CREATOR_DISCRIMINATOR,
            pump_fun::Instructions::MIGRATE_BONDING_CURVE_CREATOR_DISCRIMINATOR_OFFSET,
            "MIGRATE_BONDING_CURVE_CREATOR",
        ),
        (
            pump_fun::Instructions::SELL_DISCRIMINATOR,
            pump_fun::Instructions::SELL_DISCRIMINATOR_OFFSET,
            "SELL",
        ),
        (
            pump_fun::Instructions::SET_CREATOR_DISCRIMINATOR,
            pump_fun::Instructions::SET_CREATOR_DISCRIMINATOR_OFFSET,
            "SET_CREATOR",
        ),
        (
            pump_fun::Instructions::SET_MAYHEM_VIRTUAL_PARAMS_DISCRIMINATOR,
            pump_fun::Instructions::SET_MAYHEM_VIRTUAL_PARAMS_DISCRIMINATOR_OFFSET,
            "SET_MAYHEM_VIRTUAL_PARAMS",
        ),
        (
            pump_fun::Instructions::SET_METAPLEX_CREATOR_DISCRIMINATOR,
            pump_fun::Instructions::SET_METAPLEX_CREATOR_DISCRIMINATOR_OFFSET,
            "SET_METAPLEX_CREATOR",
        ),
        (
            pump_fun::Instructions::SET_PARAMS_DISCRIMINATOR,
            pump_fun::Instructions::SET_PARAMS_DISCRIMINATOR_OFFSET,
            "SET_PARAMS",
        ),
        (
            pump_fun::Instructions::SET_RESERVED_FEE_RECIPIENTS_DISCRIMINATOR,
            pump_fun::Instructions::SET_RESERVED_FEE_RECIPIENTS_DISCRIMINATOR_OFFSET,
            "SET_RESERVED_FEE_RECIPIENTS",
        ),
        (
            pump_fun::Instructions::SYNC_USER_VOLUME_ACCUMULATOR_DISCRIMINATOR,
            pump_fun::Instructions::SYNC_USER_VOLUME_ACCUMULATOR_DISCRIMINATOR_OFFSET,
            "SYNC_USER_VOLUME_ACCUMULATOR",
        ),
        (
            pump_fun::Instructions::TOGGLE_CASHBACK_ENABLED_DISCRIMINATOR,
            pump_fun::Instructions::TOGGLE_CASHBACK_ENABLED_DISCRIMINATOR_OFFSET,
            "TOGGLE_CASHBACK_ENABLED",
        ),
        (
            pump_fun::Instructions::TOGGLE_CREATE_V2_DISCRIMINATOR,
            pump_fun::Instructions::TOGGLE_CREATE_V2_DISCRIMINATOR_OFFSET,
            "TOGGLE_CREATE_V2",
        ),
        (
            pump_fun::Instructions::TOGGLE_MAYHEM_MODE_DISCRIMINATOR,
            pump_fun::Instructions::TOGGLE_MAYHEM_MODE_DISCRIMINATOR_OFFSET,
            "TOGGLE_MAYHEM_MODE",
        ),
        (
            pump_fun::Instructions::UPDATE_GLOBAL_AUTHORITY_DISCRIMINATOR,
            pump_fun::Instructions::UPDATE_GLOBAL_AUTHORITY_DISCRIMINATOR_OFFSET,
            "UPDATE_GLOBAL_AUTHORITY",
        ),
    ];

    assert_eq!(
        all.len(),
        29,
        "every pump_fun instruction should expose a constant"
    );

    for (disc, offset, name) in all {
        let mut data = vec![0_u8; *offset];

        data.extend_from_slice(disc);

        let path = shipstern_core::instruction::Path::new_single(0);

        if let Err(shipstern_core::ParseError::DiscriminatorNotFound(msg)) =
            pump_fun::resolve_instruction_default(&[], &data, &path)
        {
            panic!("{name}: constant is not recognised by the matcher ({msg})");
        }
    }
}

///
/// Tie the constant to real on-wire bytes, not just to the matcher.
///
/// For the instruction the matcher resolves as `Buy`, the bytes actually present
/// in the mainnet transaction at `BUY_DISCRIMINATOR_OFFSET` must equal
/// `BUY_DISCRIMINATOR`.
///
#[tokio::test]
async fn instruction_const_matches_real_mainnet_bytes() {
    let parser = pump_fun::InstructionParser;

    let fixture = match shipstern_mock::load_fixture(
        "3tkxRjNDfth6NxXpYbbLKmPkPYyAD4jjXfNnDCYtCKSPN2zSpJXT29reowKtFKz1puY1fHmBFVAskkK2A7o8cZgJ",
        &parser,
    )
    .await
    .unwrap()
    {
        shipstern_mock::FixtureData::Instructions(fixture) => fixture,
        _ => panic!("expected an instruction fixture"),
    };

    let mut checked = 0_usize;

    for raw in &fixture.instructions {
        let update: shipstern_core::instruction::InstructionUpdate = raw.into();

        if *update.program != pump_fun::PROGRAM_ID {
            continue;
        }

        let Ok(parsed) =
            pump_fun::resolve_instruction_default(&update.accounts, &update.data, &update.path)
        else {
            continue;
        };

        let pump_fun::instruction::Instruction::Buy { .. } = &parsed.instruction else {
            continue;
        };

        let offset = pump_fun::Instructions::BUY_DISCRIMINATOR_OFFSET;
        let disc = pump_fun::Instructions::BUY_DISCRIMINATOR;

        assert_eq!(
            &update.data[offset..offset + disc.len()],
            disc,
            "on-wire bytes disagree with BUY_DISCRIMINATOR"
        );

        checked += 1;
    }

    assert_eq!(checked, 1, "expected exactly one Buy in the fixture");
}

///
/// Hostile buffers against a real Anchor IDL.
///
/// `parser_robustness.rs` drives synthetic fixtures; this drives the one already
/// expanded in this binary, so it adds no compile time. pump_fun is the IDL that
/// closes the two gaps those fixtures leave: all six accounts carry an 8-byte
/// discriminator at offset 0 with no zero-width or size-only arm, so `try_unpack`
/// walks a real byte window, and the instructions declare real account lists, so a
/// short account vector actually reaches a generated accessor.
///
#[test]
fn hostile_buffers_never_panic() {
    struct Rng(u64);

    impl Rng {
        fn fill(&mut self, len: usize) -> Vec<u8> {
            (0..len)
                .map(|_| {
                    self.0 ^= self.0 >> 12;
                    self.0 ^= self.0 << 25;
                    self.0 ^= self.0 >> 27;

                    (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 24) as u8
                })
                .collect()
        }
    }

    let path = shipstern_core::instruction::Path::new_single(0);

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    let accounts: Vec<Pubkey> = (0..20)
        .map(|_| Pubkey::new(rng.fill(32).try_into().expect("32 bytes")))
        .collect();

    let disc = pump_fun::Instructions::BUY_DISCRIMINATOR;

    let mut reached = 0_usize;

    for len in 0..=96_usize {
        let garbage = rng.fill(len);

        // A random buffer satisfies an 8-byte discriminator with probability 2^-64,
        // so half the corpus is primed with a real one to get past the first compare.
        let mut primed = disc.to_vec();

        primed.extend_from_slice(&garbage);

        for data in [&garbage, &primed] {
            for n in [0_usize, 1, 8, accounts.len()] {
                if pump_fun::resolve_instruction_default(&accounts[..n], data, &path).is_ok() {
                    reached += 1;
                }
            }

            if pump_fun::PumpFunAccount::try_unpack(data).is_ok() {
                reached += 1;
            }
        }
    }

    assert!(
        reached > 0,
        "no buffer reached a decoder, so this corpus proves nothing about truncation",
    );
}
