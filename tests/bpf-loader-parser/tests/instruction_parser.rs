use shipstern_bpf_loader_parser::{instruction, InstructionParser};
use shipstern_core::{
    instruction::{InstructionUpdate, Path},
    Parser, ProgramParser, Pubkey,
};
use shipstern_mock::tx_fixture;

#[tokio::test]
async fn parse_initialize_buffer_ix() {
    let parser = InstructionParser;

    let ixs = tx_fixture!(
        "2k5ruz3otFDFtszxAPm6MmB6YSEsXLDgEMPzA9ZK3dZquXmpPCPkx5HR9r9idZKywSZTwGWZ2gAvGgVXYyfWWeFC",
        &parser
    );

    let ix0 = &ixs[0];

    let Some(instruction::Instruction::InitializeBuffer(ib)) =
        ix0.as_ref().and_then(|i| i.instruction.as_ref())
    else {
        panic!("Invalid Instruction");
    };

    let accounts = ib.accounts.as_ref().expect("missing accounts");
    assert_ne!(accounts.buffer, Pubkey::default());
    assert_ne!(accounts.authority, Some(Pubkey::default()));
    assert!(accounts.authority.is_some());
}

#[tokio::test]
async fn parse_upgrade_ix() {
    let parser = InstructionParser;

    let ixs = tx_fixture!(
        "31AUfFXG6BJQjaqwBsCjjZV5ojEL4zbrJ9gKQfKHDMosPvJKQBy6dKTiZgkkjoKbG1StD11csqgWn1KU5EwQsUgX",
        &parser
    );

    let ix0 = &ixs[0];

    let Some(instruction::Instruction::Upgrade(up)) =
        ix0.as_ref().and_then(|i| i.instruction.as_ref())
    else {
        panic!("Invalid Instruction");
    };

    let accounts = up.accounts.as_ref().expect("missing accounts");
    assert_ne!(accounts.program_data, Pubkey::default());
    assert_ne!(accounts.program, Pubkey::default());
    assert_ne!(accounts.buffer, Pubkey::default());
    assert_ne!(accounts.spill, Pubkey::default());
    assert_ne!(accounts.authority, Pubkey::default());
}

fn initialize_buffer_ix(accounts: Vec<Pubkey>) -> InstructionUpdate {
    InstructionUpdate {
        program: InstructionParser.program_id(),
        accounts,
        data: vec![0, 0, 0, 0],
        shared: Default::default(),
        inner: vec![],
        path: Path::new_single(0),
        log_range: 0..0,
    }
}

#[tokio::test]
async fn initialize_buffer_without_authority_is_immutable_buffer() {
    let buffer = Pubkey::new([7; 32]);
    let ix = initialize_buffer_ix(vec![buffer]);

    let parsed = InstructionParser
        .parse(&ix)
        .await
        .expect("a single-account InitializeBuffer must parse");

    let Some(instruction::Instruction::InitializeBuffer(ib)) = parsed.instruction else {
        panic!("Invalid Instruction");
    };
    let accounts = ib.accounts.expect("missing accounts");

    assert_eq!(accounts.buffer, buffer);
    assert_eq!(accounts.authority, None);
}

#[tokio::test]
async fn initialize_buffer_with_authority_keeps_it() {
    let authority = Pubkey::new([9; 32]);
    let ix = initialize_buffer_ix(vec![Pubkey::new([7; 32]), authority]);

    let parsed = InstructionParser.parse(&ix).await.unwrap();

    let Some(instruction::Instruction::InitializeBuffer(ib)) = parsed.instruction else {
        panic!("Invalid Instruction");
    };

    assert_eq!(ib.accounts.unwrap().authority, Some(authority));
}
