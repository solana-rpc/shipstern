use shipstern_core::{Parser, Pubkey};
use shipstern_mock::tx_fixture;
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::{check_protobuf_format, p};

// This IDL have non-even length hex strings for discriminators, which caused `hex::decode` to fail before we added padding logic. This test ensures that the padding logic works correctly.

include_shipstern_parser!("../idls/raydium_amm_v4_with_swapv2.json");

#[test]
fn check_protobuf_schema() {
    check_protobuf_format(raydium_amm::PROTOBUF_SCHEMA);

    insta::assert_snapshot!(raydium_amm::PROTOBUF_SCHEMA);
}

///
/// A CustomInstructionParser resolver telling `swapBaseIn` (18 accounts) from
/// `swapBaseInCompact` (17) by account count; both use discriminator 0x09.
///
#[tokio::test]
async fn parse_swap_base_in_with_custom_resolver() {
    use shipstern_core::ParseError;

    #[derive(Debug, Copy, Clone)]
    struct RaydiumResolver;

    impl raydium_amm::InstructionResolver for RaydiumResolver {
        fn resolve(
            &self,
            accounts: &[Pubkey],
            data: &[u8],
            path: &shipstern_core::instruction::Path,
        ) -> Result<raydium_amm::Instructions, ParseError> {
            // SwapBaseIn discriminator is 0x09 — both variants share it.
            // Disambiguate by account count.
            if data.first() == Some(&0x09) {
                return if accounts.len() >= 18 {
                    raydium_amm::parse_swap_base_in(accounts, data)
                } else {
                    raydium_amm::parse_swap_base_in_compact(accounts, data)
                };
            }

            // Everything else: default resolution
            raydium_amm::resolve_instruction_default(accounts, data, path)
        }
    }

    let parser = raydium_amm::CustomInstructionParser(RaydiumResolver);

    let ixs = tx_fixture!(
        "3J7xWK1gZTyk6GQJUopbP34Z1GxRSN1dLEmq3PyUrbMVYjkjfvMSLNRDVDggVrneJJGt1cdXsgfRUGYnavP75XQR",
        &parser
    );

    // This transaction has 17 accounts, so the resolver routes to SwapBaseInCompact
    let (swap_accounts, swap_args) = ixs
        .iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            raydium_amm::instruction::Instruction::SwapBaseInCompact { accounts, args } => {
                Some((accounts, args))
            },
            _ => None,
        })
        .expect("no SwapBaseInCompact found");

    let expected = raydium_amm::instruction::SwapBaseInCompact {
        accounts: raydium_amm::instruction::SwapBaseInCompactAccounts {
            token_program: p("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
            amm: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            amm_authority: p("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1"),
            amm_open_orders: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            pool_coin_token_account: p("AMtPGYQS873njs35mD9MAAMKoospEuzNHPy7LQuuKo4A"),
            pool_pc_token_account: p("BUvMbqP311JDU4ZGnf1rSZJLjutTU9VpNLEos393TYyW"),
            serum_program: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_market: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_bids: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_asks: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_event_queue: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_coin_vault_account: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_pc_vault_account: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            serum_vault_signer: p("5WGYajM1xtLy3QrLHGSX4YPwsso3jrjEsbU1VivUErzk"),
            user_source_token_account: p("3xGDodLWEbjPde4JedHAuGSKbM2VbqgpzK76pHpinJhL"),
            user_destination_token_account: p("BZP7eTQqi4M7XXWci3rVcM3kJsTCHqdpBewLa6aCyZVu"),
            user_source_owner: p("CsVdJ8WH8Q9eHSTRpwtwN3TYApm24QnLKYUMNxJ3DaED"),
            remaining_accounts: vec![],
        },
        args: raydium_amm::instruction::SwapBaseInCompactArgs {
            amount_in: 820_106_078_370,
            minimum_amount_out: 0,
        },
    };

    assert_eq!(swap_accounts, &expected.accounts);
    assert_eq!(swap_args, &expected.args);
}

///
/// The default InstructionParser now automatically disambiguates instructions
/// sharing a discriminator when they have different account counts.
///
/// For raydium_amm, `swapBaseIn` (18 accounts) and `swapBaseInCompact` (17 accounts)
/// both share discriminator 0x09. The default resolver picks the right one by checking
/// `accounts.len() >= 18` first, then falling back to `swapBaseInCompact`.
///
#[tokio::test]
async fn parse_swap_base_in_with_default_parser() {
    let parser = raydium_amm::InstructionParser;

    let ixs = tx_fixture!(
        "3J7xWK1gZTyk6GQJUopbP34Z1GxRSN1dLEmq3PyUrbMVYjkjfvMSLNRDVDggVrneJJGt1cdXsgfRUGYnavP75XQR",
        &parser
    );

    // This transaction has 17 accounts — the default resolver should route
    // to SwapBaseInCompact automatically.
    ixs.iter()
        .find_map(|ix| match &ix.as_ref()?.instruction {
            raydium_amm::instruction::Instruction::SwapBaseInCompact { accounts, args } => {
                Some((accounts, args))
            },
            _ => None,
        })
        .expect("default parser should resolve SwapBaseInCompact by account count");
}

#[test]
fn check_json_serialization() {
    // account
    let config = raydium_amm::AmmConfig::default();
    let json_str = serde_json::to_string(&config).expect("failed to json serialize");
    let _: raydium_amm::AmmConfig =
        serde_json::from_str(&json_str).expect("failed to json deserialize");

    // instruction
    let ix = raydium_amm::instruction::CreateConfigAccount::default();
    let json_str = serde_json::to_string(&ix).expect("failed to json serialize");
    let _: raydium_amm::instruction::CreateConfigAccount =
        serde_json::from_str(&json_str).expect("failed to json deserialize");
}
