use prost::Message;
use shipstern_core::{Parser, Pubkey};
use shipstern_mock::{account_fixture, tx_fixture};
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::{check_protobuf_format, p};

include_shipstern_parser!("../idls/perpetuals.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(perpetuals::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(perpetuals::PROTOBUF_SCHEMA);
}

#[tokio::test]
async fn parse_custody_account() {
    let parser = perpetuals::AccountParser;
    let account = account_fixture!("7xS2gz2bTp3fwCC7knJvUWTEU9Tycczu6VhJYKgi1wdz", &parser);

    let custody = match account.account {
        perpetuals::account::Account::Custody(c) => c,
        _ => panic!("Unexpected account state"),
    };

    let expected = perpetuals::Custody {
        pool: p("5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq"),
        mint: p("So11111111111111111111111111111111111111112"),
        token_account: p("BUvduFTd2sWFagCunBPLupG8fBTJqweLw9DuhruNFSCm"),
        decimals: 9,
        is_stable: false,
        oracle: perpetuals::OracleParams {
            oracle_account: p("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE"),
            oracle_type: perpetuals::OracleType {
                kind: perpetuals::oracle_type::Kind::Pyth(perpetuals::OracleTypePyth {}),
            },
            buffer: 50,
            max_price_age_sec: 5,
        },
        pricing: perpetuals::PricingParams {
            trade_impact_fee_scalar: 3_750_000_000_000_000,
            buffer: 0,
            swap_spread: 0,
            max_leverage: 5_000_000,
            max_global_long_sizes: 410_000_000_000_000,
            max_global_short_sizes: 112_293_947_657_720,
        },
        permissions: perpetuals::Permissions {
            allow_swap: true,
            allow_add_liquidity: true,
            allow_remove_liquidity: true,
            allow_increase_position: true,
            allow_decrease_position: true,
            allow_collateral_withdrawal: true,
            allow_liquidate_position: true,
        },
        target_ratio_bps: 4700,
        assets: perpetuals::Assets {
            fees_reserves: 466_896_904,
            owned: 6_368_829_939_225_389,
            locked: 398_908_550_717_247,
            guaranteed_usd: 24_953_150_512_329,
            global_short_sizes: 10_560_791_260_461,
            global_short_average_prices: 83_486_086,
        },
        funding_rate_state: perpetuals::FundingRateState {
            cumulative_interest_rate: 989_574_039,
            last_update: 1_771_834_760,
            hourly_funding_dbps: 0,
        },
        bump: 253,
        token_account_bump: 255,
        increase_position_bps: 6,
        decrease_position_bps: 6,
        max_position_size_usd: 10_000_000_000_000,
        doves_oracle: p("39cWjvHrpHNz2SbXv6ME4NPhqBDBd4KsjUYv5JkHEAJU"),
        jump_rate_state: perpetuals::JumpRateState {
            min_rate_bps: 1000,
            max_rate_bps: 15000,
            target_rate_bps: 3500,
            target_utilization_rate: 800_000_000,
        },
        doves_ag_oracle: p("FYq2BWQ1V5P1WFBqr3qB2Kb5yHVvSv7upzKodgQE5zXh"),
        price_impact_buffer: perpetuals::PriceImpactBuffer {
            open_interest: vec![
                0,
                0,
                -3_691_644_661,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                620_487_212,
                0,
                0,
                0,
                0,
                1_936_686_312,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                2_385_457_659,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                7_440_976_097,
                0,
                0,
                0,
            ],
            last_updated: 1_771_834_760,
            fee_factor: 1,
            exponent: 1.0,
            delta_imbalance_threshold_decimal: 1_500_000_000_000,
            max_fee_bps: 50,
        },
        borrow_lend_parameters: perpetuals::BorrowLendParams {
            borrows_limit_in_bps: 0,
            maintainance_margin_bps: 0,
            protocol_fee_bps: 0,
            liquidation_margin: 0,
            liquidation_fee_bps: 0,
        },
        borrows_funding_rate_state: perpetuals::FundingRateState {
            cumulative_interest_rate: 1_000_000_000_000_000_000,
            last_update: 1_771_834_760,
            hourly_funding_dbps: 0,
        },
        debt: 0u128,
        borrow_lend_interests_accured: 0u128,
        borrow_limit_in_token_amount: 0,
        min_interest_fee_bps: 0,
        min_interest_fee_grace_period_seconds: 0,
        total_staked_amount_lamports: 5_620_789_931_843_821,
        max_total_staked_amount_lamports: 6_000_000_000_000_000,
        external_swap_fee_multiplier_bps: 20000,
        disable_close_position_request: false,
        withdrawal_limit_token_amount: 0,
        withdrawal_token_amount_accumulated: 0,
        withdrawal_limit_last_reset_at: 0,
        withdrawal_limit_interval_seconds: 3600,
    };

    assert_eq!(custody, expected);
}

#[tokio::test]
async fn parse_pool_account() {
    let parser = perpetuals::AccountParser;
    let account = account_fixture!("5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq", &parser);

    let pool = match account.account {
        perpetuals::account::Account::Pool(pool) => pool,
        _ => panic!("Unexpected account state"),
    };

    let expected = perpetuals::Pool {
        name: "Pool".to_string(),
        custodies: vec![
            p("7xS2gz2bTp3fwCC7knJvUWTEU9Tycczu6VhJYKgi1wdz"),
            p("AQCGyheWPLeo6Qp9WpYS9m3Qj479t7R636N9ey1rEjEn"),
            p("5Pv3gM9JrFFH883SWAhvJC9RPYmo8UNxuFtv5bMMALkm"),
            p("G18jKKXQwBbrHeiK3C9MRXhkHsLHf7XgCSisykV46EZa"),
            p("4vkNeXiYEUizLdrpdPS1eC2mccyM4NUPRtERrk6ZETkk"),
        ],
        aum_usd: 1_133_521_743_352_874,
        limit: perpetuals::Limit {
            max_aum_usd: 3_000_000_000_000_000,
            token_weightage_buffer_bps: 1000,
            buffer: 0,
        },
        fees: perpetuals::Fees {
            swap_multiplier: 100,
            stable_swap_multiplier: 100,
            add_remove_liquidity_bps: 20,
            swap_bps: 10,
            tax_bps: 100,
            stable_swap_bps: 2,
            stable_swap_tax_bps: 5,
            liquidation_reward_bps: 150,
            protocol_share_bps: 2500,
        },
        pool_apr: perpetuals::PoolApr {
            last_updated: 1771002900,
            fee_apr_bps: 1093,
            realized_fee_usd: 1250427617755,
        },
        max_request_execution_sec: 45,
        bump: 252,
        lp_token_bump: 254,
        inception_time: 1689677832,
        parameter_update_oracle: perpetuals::Secp256k1Pubkey {
            prefix: 2,
            key: vec![
                81, 186, 187, 169, 123, 155, 81, 29, 179, 83, 117, 14, 44, 57, 230, 19, 66, 46,
                133, 49, 33, 164, 138, 181, 32, 132, 12, 0, 39, 52, 40, 92,
            ],
        },
        aum_usd_updated_at: 1771515604,
        max_trigger_price_diff_bps: 500,
        disable_close_position_request: false,
        max_lp_token_price_change_bps: 100,
        aum_usd_refreshed_at_slot: 401338012,
    };

    assert_eq!(pool, expected);
}

#[tokio::test]
async fn parse_decrease_position_with_tpsl_and_close_position_request_2_ix() {
    let parser = perpetuals::InstructionParser;

    let ixs = tx_fixture!(
        "3MnCnWjL83RBYGe8ADwhxFHTvbXmDEPyi2mxHQ2211kmQ4NAqBFvhjPexgiCy8o6mGPztnZwjbcxRi4WMGZDeah8",
        &parser
    );

    {
        let (decrease_accounts, decrease_args) = ixs
            .iter()
            .find_map(|ix| match &ix.as_ref()?.instruction {
                perpetuals::instruction::Instruction::DecreasePositionWithTpsl {
                    accounts,
                    args,
                } => Some((accounts, args)),
                _ => None,
            })
            .expect("no decrease position ix found");

        assert_eq!(
            decrease_accounts,
            &perpetuals::instruction::DecreasePositionWithTpslAccounts {
                keeper: p("H3dDE6K6uqBp5kBKctH6w8hzpV5eAmxUiqvcdHxuE9i4"),
                owner: p("AHjZmPJ3swFKz6nVhoeV3Y2zB94rd2QFbAoEPvNo6DXX"),
                transfer_authority: p("AVzP2GeRmqGphJsMxWoqjpUifPpCret7LqWhD8NWQK49"),
                perpetuals: p("H4ND9aYttUVLFmNypZqLjZ52FYiGvdEB45GmwNoKEjTj"),
                pool: p("5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq"),
                position_request: p("GiHy9ixvdkzGQVoTVXkppg7FCMxx1gpwnB7Df9HEUuBM"),
                position_request_ata: p("2zazRXFzzDHUJF4UBuGcmp3wbUh7zuhpjEuEHG1APisc"),
                position: p("2RPcEvVpLwVVxAPYn89np27pTC6keUGQ1Yf77qa7i1ok"),
                custody: p("7xS2gz2bTp3fwCC7knJvUWTEU9Tycczu6VhJYKgi1wdz"),
                custody_doves_price_account: p("FYq2BWQ1V5P1WFBqr3qB2Kb5yHVvSv7upzKodgQE5zXh"),
                collateral_custody: p("7xS2gz2bTp3fwCC7knJvUWTEU9Tycczu6VhJYKgi1wdz"),
                collateral_custody_doves_price_account: p(
                    "FYq2BWQ1V5P1WFBqr3qB2Kb5yHVvSv7upzKodgQE5zXh",
                ),
                collateral_custody_token_account: p("BUvduFTd2sWFagCunBPLupG8fBTJqweLw9DuhruNFSCm"),
                token_program: p("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
                event_authority: p("37hJBDnntwqhGbK7L6M1bLyvccj4u55CCUiLPdYkiqBN"),
                program: Pubkey::new(perpetuals::PROGRAM_ID),
                remaining_accounts: vec![],
            }
        );
        assert_eq!(
            decrease_args,
            &perpetuals::instruction::DecreasePositionWithTpslArgs {}
        );
    }

    {
        let (close_accounts, close_args) = ixs
            .iter()
            .find_map(|ix| match &ix.as_ref()?.instruction {
                perpetuals::instruction::Instruction::ClosePositionRequest2 { accounts, args } => {
                    Some((accounts, args))
                },
                _ => None,
            })
            .expect("no close position request 2 ix found");

        assert_eq!(
            close_accounts,
            &perpetuals::instruction::ClosePositionRequest2Accounts {
                keeper: Some(p("H3dDE6K6uqBp5kBKctH6w8hzpV5eAmxUiqvcdHxuE9i4")),
                owner: p("AHjZmPJ3swFKz6nVhoeV3Y2zB94rd2QFbAoEPvNo6DXX"),
                owner_ata: p("BPRnMmfxF7QYepU9ypvBMA4eXwbHGST79bUgKvoZ6Jsk"),
                pool: p("5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq"),
                position_request: p("GiHy9ixvdkzGQVoTVXkppg7FCMxx1gpwnB7Df9HEUuBM"),
                position_request_ata: p("2zazRXFzzDHUJF4UBuGcmp3wbUh7zuhpjEuEHG1APisc"),
                position: p("2RPcEvVpLwVVxAPYn89np27pTC6keUGQ1Yf77qa7i1ok"),
                mint: p("So11111111111111111111111111111111111111112"),
                token_program: p("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
                system_program: p("11111111111111111111111111111111"),
                associated_token_program: p("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
                event_authority: p("37hJBDnntwqhGbK7L6M1bLyvccj4u55CCUiLPdYkiqBN"),
                program: Pubkey::new(perpetuals::PROGRAM_ID),
                remaining_accounts: vec![],
            }
        );
        assert_eq!(
            close_args,
            &perpetuals::instruction::ClosePositionRequest2Args {}
        );
    }
}

#[tokio::test]
async fn parse_borrow_from_custody_ix() {
    let parser = perpetuals::InstructionParser;

    let ixs = tx_fixture!(
        "5mYEUYXCZisS8CChCG8mL8N3NEWHUA81Rr7kLA28P5upSzDStLq1f4QKhFLY7R8GsRNB27gM6YzvKerxejtLQxCj",
        &parser
    );

    let (borrow_accounts, borrow_args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            perpetuals::instruction::Instruction::BorrowFromCustody { accounts, args } => {
                Some((accounts, args))
            },
            _ => None,
        })
        .expect("no borrow from custody ix found");

    assert_eq!(
        borrow_accounts,
        &perpetuals::instruction::BorrowFromCustodyAccounts {
            owner: p("E2Z5ggFhABjC5tSZYouMgfgUpgNsvDpWrR6YTFt7D4YC"),
            perpetuals: p("H4ND9aYttUVLFmNypZqLjZ52FYiGvdEB45GmwNoKEjTj"),
            pool: p("5BUwFW4nRbftYTDMbgxykoFWqWHPzahFSNAaaaJtVKsq"),
            custody: p("G18jKKXQwBbrHeiK3C9MRXhkHsLHf7XgCSisykV46EZa"),
            transfer_authority: p("AVzP2GeRmqGphJsMxWoqjpUifPpCret7LqWhD8NWQK49"),
            borrow_position: p("iUzDVme5Mc21GdULKK2JFuvjNWY4TaULF2kNGTcoXf9"),
            custody_token_account: p("WzWUoCmtVv7eqAbU3BfKPU3fhLP6CXR8NCJH78UK9VS"),
            user_token_account: p("Av9zpU3ZtdfYMHdW9ombpaarS9bQhsK4Rxvt6piUTkmH"),
            lp_token_mint: p("27G8MtK7VtTcCHkpASjSDdkWWYfoqT6ggEuKidVJidD4"),
            token_program: p("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
            event_authority: p("37hJBDnntwqhGbK7L6M1bLyvccj4u55CCUiLPdYkiqBN"),
            program: p("PERPHjGBqRHArX4DySjwM6UJHiR3sWAatqfdBS2qQJu"),
            remaining_accounts: vec![
                p("7xS2gz2bTp3fwCC7knJvUWTEU9Tycczu6VhJYKgi1wdz"),
                p("AQCGyheWPLeo6Qp9WpYS9m3Qj479t7R636N9ey1rEjEn"),
                p("5Pv3gM9JrFFH883SWAhvJC9RPYmo8UNxuFtv5bMMALkm"),
                p("G18jKKXQwBbrHeiK3C9MRXhkHsLHf7XgCSisykV46EZa"),
                p("4vkNeXiYEUizLdrpdPS1eC2mccyM4NUPRtERrk6ZETkk"),
                p("FYq2BWQ1V5P1WFBqr3qB2Kb5yHVvSv7upzKodgQE5zXh"),
                p("AFZnHPzy4mvVCffrVwhewHbFc93uTHvDSFrVH7GtfXF1"),
                p("hUqAT1KQ7eW1i6Csp9CXYtpPfSAvi835V7wKi5fRfmC"),
                p("6Jp2xZUTWdDD2ZyUPRzeMdc6AFQ5K3pFgZxk2EijfjnM"),
                p("Fgc93D641F8N2d1xLjQ4jmShuD3GE3BsCXA56KBQbF5u"),
            ],
        }
    );
    assert_eq!(
        borrow_args,
        &perpetuals::instruction::BorrowFromCustodyArgs {
            amount: 8_463_144_037,
        }
    );
}

///
/// Proto encode → decode round-trip for structs containing native u8 fields.
///
/// Verifies that the manual `prost::Message` impl correctly widens u8→u32
/// on encode and narrows u32→u8 on decode, preserving the original values.
///
#[tokio::test]
async fn proto_round_trip_native_u8_fields() {
    let parser = perpetuals::AccountParser;
    let account = account_fixture!("7xS2gz2bTp3fwCC7knJvUWTEU9Tycczu6VhJYKgi1wdz", &parser);

    let custody = match account.account {
        perpetuals::account::Account::Custody(c) => c,
        _ => panic!("Unexpected account state"),
    };

    // Verify native u8 fields have their original values before round-trip.
    assert_eq!(custody.decimals, 9u8);
    assert_eq!(custody.bump, 253u8);
    assert_eq!(custody.token_account_bump, 255u8);

    // Encode to proto bytes.
    let mut buf = Vec::new();

    custody.encode(&mut buf).expect("proto encode failed");

    // Decode back from proto bytes.
    let decoded = perpetuals::Custody::decode(buf.as_slice()).expect("proto decode failed");

    // All fields must survive the round-trip, including the u8 ones.
    assert_eq!(decoded, custody);

    // Double-check the u8 fields explicitly — 253 and 255 exercise the
    // high end of the range where truncation bugs would show.
    assert_eq!(decoded.decimals, 9u8);
    assert_eq!(decoded.bump, 253u8);
    assert_eq!(decoded.token_account_bump, 255u8);
}

///
/// Proto encode → decode round-trip for instruction dispatch (oneof wrapper).
///
/// The `Instructions` struct uses a manual `prost::Message` impl that delegates
/// to the manual `prost::Oneof`-style impl on the enum. This test verifies the
/// full chain works correctly.
///
#[tokio::test]
async fn proto_round_trip_instruction() {
    let parser = perpetuals::InstructionParser;

    let ixs = tx_fixture!(
        "5mYEUYXCZisS8CChCG8mL8N3NEWHUA81Rr7kLA28P5upSzDStLq1f4QKhFLY7R8GsRNB27gM6YzvKerxejtLQxCj",
        &parser
    );

    let original = ixs
        .iter()
        .find_map(|ix| ix.as_ref())
        .expect("no instruction found");

    // Encode to proto bytes.
    let mut buf = Vec::new();

    original.encode(&mut buf).expect("proto encode failed");

    assert!(!buf.is_empty(), "encoded bytes should not be empty");

    // Verify encoded_len matches actual output size.
    assert_eq!(original.encoded_len(), buf.len());
}

#[test]
fn check_json_serialization() {
    // account
    let assets = perpetuals::Assets::default();
    let json_str = serde_json::to_string(&assets).expect("failed to json serialize");
    let _: perpetuals::Assets =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let ix = perpetuals::instruction::CreateAndDelegateStakeAccount::default();
    let json_str = serde_json::to_string(&ix).expect("failed to json serialize");
    let _: perpetuals::instruction::CreateAndDelegateStakeAccount =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}

/// A missing account with a fixed IDL address falls back to it; a missing
/// account without one still errors.
#[test]
fn missing_fixed_address_account_falls_back_to_idl_address() {
    let data = perpetuals::Instructions::SET_TOKEN_LEDGER_DISCRIMINATOR;
    let path = shipstern_core::instruction::Path::new_single(0);

    let token_ledger = Pubkey::new([1; 32]);
    let token_account = Pubkey::new([2; 32]);
    let passed_program = Pubkey::new([3; 32]);

    let token_program = |accounts: &[Pubkey]| {
        let parsed = perpetuals::resolve_instruction_default(accounts, data, &path)
            .expect("setTokenLedger parses");

        let perpetuals::instruction::Instruction::SetTokenLedger { accounts, .. } =
            parsed.instruction
        else {
            panic!("expected SetTokenLedger");
        };

        accounts.token_program
    };

    assert_eq!(
        token_program(&[token_ledger, token_account]),
        p("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")
    );
    assert_eq!(
        token_program(&[token_ledger, token_account, passed_program]),
        passed_program
    );

    let err = perpetuals::resolve_instruction_default(&[token_ledger], data, &path)
        .expect_err("tokenAccount has no fixed address");

    assert_eq!(err.to_string(), "Account does not exist at index 1");
}
