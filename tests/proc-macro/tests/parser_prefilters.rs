//!
//! The runtime merges every instruction parser into one server filter, so each parser
//! must select its program with `include`. A `required` key would make every parser need it.
//!

use shipstern_core::{ParseError, Parser, Prefilter, ProgramParser, Pubkey};
use shipstern_proc_macro::include_shipstern_parser;
use shipstern_test_utils::p;

include_shipstern_parser!("../idls/size_only_instructions.json");

#[derive(Debug, Clone, Copy)]
struct DefaultResolver;

impl size_only_instructions::InstructionResolver for DefaultResolver {
    fn resolve(
        &self,
        accounts: &[Pubkey],
        data: &[u8],
        path: &shipstern_core::instruction::Path,
    ) -> Result<size_only_instructions::Instructions, ParseError> {
        size_only_instructions::resolve_instruction_default(accounts, data, path)
    }
}

fn assert_selects_with_include(name: &str, prefilter: Prefilter, program: Pubkey) {
    let tx = prefilter.transaction.expect(name);

    assert_eq!(tx.accounts_include, [program].into(), "{name}");
    assert!(tx.accounts_required.is_empty(), "{name}");
}

fn check<P: ProgramParser>(name: &str, parser: &P) {
    assert_selects_with_include(name, parser.prefilter(), parser.program_id());
}

#[test]
fn instruction_parsers_select_their_program_with_include() {
    check("generated", &size_only_instructions::InstructionParser);
    check(
        "generated custom",
        &size_only_instructions::CustomInstructionParser(DefaultResolver),
    );
    check("spl-token", &shipstern_spl_token_parser::InstructionParser);
    check(
        "token-2022",
        &shipstern_spl_token_extensions_parser::InstructionParser,
    );
    check(
        "bpf-loader",
        &shipstern_bpf_loader_parser::InstructionParser,
    );

    assert_selects_with_include(
        "stake-pool",
        shipstern_stake_pool_parser::InstructionParser.prefilter(),
        p("SPoo1Ku8WFXoNDMHPsrGSTSG1Y47rzgn41SLUNakuHy"),
    );
}
