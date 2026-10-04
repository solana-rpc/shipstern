use shipstern_core::Pubkey;
use shipstern_proc_macro::include_shipstern_parser;

include_shipstern_parser!("../idls/wide_number_discriminator.json");

use wide_number_discriminator::instruction::Instruction;

/// A `u32` or `i16` instruction index and a `u16` account type take their full
/// width: the arguments start after the index and the account matches on both bytes.
#[test]
fn number_discriminators_use_their_declared_width() {
    let accounts = [Pubkey::new([1; 32]), Pubkey::new([2; 32])];
    let path = shipstern_core::instruction::Path::new_single(0);

    {
        let mut data = 2_u32.to_le_bytes().to_vec();
        data.extend_from_slice(&241_082_u64.to_le_bytes());

        let parsed =
            wide_number_discriminator::resolve_instruction_default(&accounts, &data, &path)
                .expect("transferSol should parse");

        let Instruction::TransferSol { args, .. } = parsed.instruction else {
            panic!("expected transferSol");
        };

        assert_eq!(args.amount, 241_082);
    }

    {
        let mut data = 8_i16.to_le_bytes().to_vec();
        data.extend_from_slice(&4_096_u64.to_le_bytes());

        let parsed =
            wide_number_discriminator::resolve_instruction_default(&accounts[..1], &data, &path)
                .expect("allocate should parse");

        let Instruction::Allocate { args, .. } = parsed.instruction else {
            panic!("expected allocate");
        };

        assert_eq!(args.space, 4_096);
    }

    let mut account = 513_u16.to_le_bytes().to_vec();
    account.extend_from_slice(&7_u64.to_le_bytes());

    let parsed = wide_number_discriminator::WideNumberDiscriminatorAccount::try_unpack(&account)
        .expect("config should match on both bytes");

    let wide_number_discriminator::account::Account::Config(config) = parsed.account;

    assert_eq!(config.value, 7);

    let mut other = 1_u16.to_le_bytes().to_vec();
    other.extend_from_slice(&7_u64.to_le_bytes());

    assert!(wide_number_discriminator::WideNumberDiscriminatorAccount::try_unpack(&other).is_err());
}
