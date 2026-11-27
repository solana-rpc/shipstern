use shipstern_core::{
    instruction::InstructionUpdate, ParseError, ParseResult, Parser, Prefilter, ProgramParser,
};
use shipstern_parser::{check_min_accounts_req, Error, Result, ResultExt};
use shipstern_spl_token_parser::{
    InstructionParser as TokenProgramInstructionParser, SetAuthorityAccounts,
};
use spl_token_2022::instruction::TokenInstruction as SplTokenInstruction;
use spl_token_group_interface::instruction::TokenGroupInstruction as SplTokenGroupInstruction;
use spl_token_metadata_interface::instruction::TokenMetadataInstruction as SplTokenMetadataInstruction;

use crate::{
    extensions::{
        CommonExtensionInstructions, ConfidentialTransferFeeIx, ConfidentialTransferIx,
        ExtensionWithCommonInstruction, TokenGroupIx, TokenMetadataIx, TransferFeeIx,
    },
    instructions::{
        CreateNativeMintAccounts, InitializeMintCloseAuthorityAccounts,
        InitializeMintCloseAuthorityArgs, InitializeNonTransferableMintAccounts,
        InitializePermanentDelegateAccounts, InitializePermanentDelegateArgs, ReallocateAccounts,
        ReallocateArgs, SetAuthorityArgs, WithdrawExcessLamportsAccounts,
    },
    AuthorityType, ExtensionInstructionParser, TokenExtensionProgram,
};

#[derive(Debug, Clone, Copy)]
pub struct InstructionParser;

impl Parser for InstructionParser {
    type Input = InstructionUpdate;
    type Output = TokenExtensionProgram;

    fn id(&self) -> std::borrow::Cow<'static, str> { "token_extensions::InstructionParser".into() }

    fn prefilter(&self) -> Prefilter {
        Prefilter::builder()
            .transaction_accounts_include([spl_token_2022::ID])
            .build()
            .unwrap()
    }

    async fn parse(&self, ix_update: &InstructionUpdate) -> ParseResult<Self::Output> {
        if !ix_update.program.equals_ref(spl_token_2022::ID) {
            return Err(ParseError::Filtered);
        }

        InstructionParser::parse_impl(ix_update).map_err(|e| ParseError::Other(e.into()))
    }
}

impl ProgramParser for InstructionParser {
    fn program_id(&self) -> shipstern_core::Pubkey { spl_token_2022::ID.to_bytes().into() }
}

impl InstructionParser {
    #[allow(clippy::too_many_lines)]
    fn parse_impl(ix: &InstructionUpdate) -> Result<TokenExtensionProgram> {
        let accounts_len = ix.accounts.len();

        macro_rules! envelope {
            ($ix:expr) => {
                TokenExtensionProgram {
                    instruction: Some($ix),
                }
            };
        }

        match SplTokenInstruction::unpack(&ix.data) {
            Ok(token_ix) => match token_ix {
                SplTokenInstruction::TransferFeeExtension => {
                    let parsed = TransferFeeIx::try_parse(ix)?;

                    Ok(envelope!(crate::instruction::Instruction::TransferFee(
                        crate::instruction::TransferFee {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::ConfidentialTransferExtension => {
                    let parsed = ConfidentialTransferIx::try_parse(ix)?;

                    Ok(envelope!(
                        crate::instruction::Instruction::ConfidentialTransfer(
                            crate::instruction::ConfidentialTransfer {
                                instruction: Some(parsed),
                            },
                        )
                    ))
                },

                SplTokenInstruction::ConfidentialTransferFeeExtension => {
                    let parsed = ConfidentialTransferFeeIx::try_parse(ix)?;

                    Ok(envelope!(
                        crate::instruction::Instruction::ConfidentialTransferFee(
                            crate::instruction::ConfidentialTransferFee {
                                instruction: Some(parsed),
                            },
                        )
                    ))
                },

                SplTokenInstruction::CpiGuardExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::CpiGuard,
                        ix,
                    )?;

                    Ok(envelope!(crate::instruction::Instruction::CpiGuard(
                        crate::instruction::CpiGuard {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::DefaultAccountStateExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::DefaultAccountState,
                        ix,
                    )?;

                    Ok(envelope!(
                        crate::instruction::Instruction::DefaultAccountState(
                            crate::instruction::DefaultAccountState {
                                instruction: Some(parsed),
                            },
                        )
                    ))
                },

                SplTokenInstruction::InterestBearingMintExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::InterestBearingMint,
                        ix,
                    )?;

                    Ok(envelope!(
                        crate::instruction::Instruction::InterestBearingMint(
                            crate::instruction::InterestBearingMint {
                                instruction: Some(parsed),
                            },
                        )
                    ))
                },

                SplTokenInstruction::MemoTransferExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::MemoTransfer,
                        ix,
                    )?;

                    Ok(envelope!(crate::instruction::Instruction::MemoTransfer(
                        crate::instruction::MemoTransfer {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::GroupMemberPointerExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::GroupMemberPointer,
                        ix,
                    )?;

                    Ok(envelope!(
                        crate::instruction::Instruction::GroupMemberPointer(
                            crate::instruction::GroupMemberPointer {
                                instruction: Some(parsed),
                            },
                        )
                    ))
                },

                SplTokenInstruction::GroupPointerExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::GroupPointer,
                        ix,
                    )?;

                    Ok(envelope!(crate::instruction::Instruction::GroupPointer(
                        crate::instruction::GroupPointer {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::MetadataPointerExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::MetadataPointer,
                        ix,
                    )?;

                    Ok(envelope!(crate::instruction::Instruction::MetadataPointer(
                        crate::instruction::MetadataPointer {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::TransferHookExtension => {
                    let parsed = CommonExtensionInstructions::try_parse_extension_instruction(
                        ExtensionWithCommonInstruction::TransferHook,
                        ix,
                    )?;

                    Ok(envelope!(crate::instruction::Instruction::TransferHook(
                        crate::instruction::TransferHook {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::SetAuthority {
                    authority_type,
                    new_authority,
                } => {
                    check_min_accounts_req(accounts_len, 2)?;

                    let accounts = SetAuthorityAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        current_authority: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    };

                    let args = SetAuthorityArgs {
                        authority_type: AuthorityType::from(authority_type) as i32,
                        new_authority: new_authority
                            .map(|pk| crate::Pubkey::new(pk.to_bytes()))
                            .into(),
                    };

                    Ok(envelope!(crate::instruction::Instruction::SetAuthority(
                        crate::instruction::SetAuthority { accounts, args },
                    )))
                },

                SplTokenInstruction::CreateNativeMint => {
                    check_min_accounts_req(accounts_len, 2)?;

                    let accounts = CreateNativeMintAccounts {
                        funding_account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                    };

                    Ok(envelope!(
                        crate::instruction::Instruction::CreateNativeMint(
                            crate::instruction::CreateNativeMint { accounts },
                        )
                    ))
                },

                SplTokenInstruction::InitializeMintCloseAuthority { close_authority } => {
                    check_min_accounts_req(accounts_len, 1)?;

                    let accounts = InitializeMintCloseAuthorityAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                    };

                    let args = InitializeMintCloseAuthorityArgs {
                        close_authority: close_authority
                            .map(|pk| crate::Pubkey::new(pk.to_bytes()))
                            .into(),
                    };

                    Ok(envelope!(
                        crate::instruction::Instruction::InitializeMintCloseAuthority(
                            crate::instruction::InitializeMintCloseAuthority { accounts, args },
                        )
                    ))
                },

                SplTokenInstruction::InitializeNonTransferableMint => {
                    check_min_accounts_req(accounts_len, 1)?;

                    let accounts = InitializeNonTransferableMintAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                    };

                    Ok(envelope!(
                        crate::instruction::Instruction::InitializeNonTransferableMint(
                            crate::instruction::InitializeNonTransferableMint { accounts },
                        )
                    ))
                },

                SplTokenInstruction::Reallocate { extension_types } => {
                    check_min_accounts_req(accounts_len, 4)?;

                    let accounts = ReallocateAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        payer: crate::Pubkey::new(ix.accounts[1].0),
                        owner: crate::Pubkey::new(ix.accounts[3].0),
                        multisig_signers: ix.accounts[4..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    };

                    let args = ReallocateArgs {
                        extension_types: extension_types.into_iter().map(|t| t as u32).collect(),
                    };

                    Ok(envelope!(crate::instruction::Instruction::Reallocate(
                        crate::instruction::Reallocate { accounts, args },
                    )))
                },

                SplTokenInstruction::InitializePermanentDelegate { delegate } => {
                    check_min_accounts_req(accounts_len, 1)?;

                    let accounts = InitializePermanentDelegateAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                    };

                    let args = InitializePermanentDelegateArgs {
                        delegate: Some(crate::Pubkey::new(delegate.to_bytes())),
                    };

                    Ok(envelope!(
                        crate::instruction::Instruction::InitializePermanentDelegate(
                            crate::instruction::InitializePermanentDelegate { accounts, args },
                        )
                    ))
                },

                SplTokenInstruction::WithdrawExcessLamports => {
                    check_min_accounts_req(accounts_len, 3)?;

                    let accounts = WithdrawExcessLamportsAccounts {
                        source_account: crate::Pubkey::new(ix.accounts[0].0),
                        destination_account: crate::Pubkey::new(ix.accounts[1].0),
                        authority: crate::Pubkey::new(ix.accounts[2].0),
                        multisig_signers: ix.accounts[3..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    };

                    Ok(envelope!(
                        crate::instruction::Instruction::WithdrawExcessLamports(
                            crate::instruction::WithdrawExcessLamports { accounts },
                        )
                    ))
                },

                // Anything else: fallback to SPL token parser
                _ => {
                    let token_instruction = TokenProgramInstructionParser::parse_impl(ix)
                        .parse_err(
                            "Error parsing token extension instruction as token instruction",
                        )?;

                    Ok(envelope!(crate::instruction::Instruction::TokenProgram(
                        crate::instruction::TokenProgram {
                            instruction: Some(token_instruction),
                        },
                    )))
                },
            },

            Err(e) => {
                if SplTokenMetadataInstruction::unpack(&ix.data).is_ok() {
                    let parsed = TokenMetadataIx::try_parse(ix)?;

                    return Ok(envelope!(crate::instruction::Instruction::TokenMetadata(
                        crate::instruction::TokenMetadata {
                            instruction: Some(parsed),
                        },
                    )));
                }

                if SplTokenGroupInstruction::unpack(&ix.data).is_ok() {
                    let parsed = TokenGroupIx::try_parse(ix)?;

                    return Ok(envelope!(crate::instruction::Instruction::TokenGroup(
                        crate::instruction::TokenGroup {
                            instruction: Some(parsed),
                        },
                    )));
                }

                Err(Error::from_inner("Error unpacking instruction data", e))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Mul;

    use shipstern_core::instruction::Path;
    use shipstern_mock::tx_fixture;

    use super::{
        InstructionParser, InstructionUpdate, ParseError, Parser, ProgramParser,
        TokenExtensionProgram,
    };

    #[tokio::test]
    async fn test_mint_to_checked_ix_parsing() {
        let parser = InstructionParser;

        let ixs = tx_fixture!(
            "44gWEyKUkeUabtJr4eT3CQEkFGrD4jMdwUV6Ew5MR5K3RGizs9iwbkb5Q4T3gnAaSgHxn3ERQ8g5YTXuLP1FrWnt",
            &parser
        );

        let Some(TokenExtensionProgram {
            instruction:
                Some(crate::instruction::Instruction::TokenProgram(
                    crate::instruction::TokenProgram {
                        instruction:
                            Some(shipstern_spl_token_parser::TokenProgram {
                                instruction: Some(shipstern_spl_token_parser::instruction::Instruction::MintToChecked(
                                    shipstern_spl_token_parser::instruction::MintToChecked { args, .. }
                                )),
                            }),
                    },
                )),
        }) = &ixs[0]
        else {
            panic!("Invalid Instruction");
        };

        assert_eq!(args.decimals, 9);
        assert_eq!(args.amount, 100.mul(10u64.pow(args.decimals)));
    }

    fn key(n: u8) -> solana_pubkey::Pubkey { solana_pubkey::Pubkey::new_from_array([n; 32]) }

    fn update_from(ix: &solana_instruction::Instruction) -> InstructionUpdate {
        InstructionUpdate {
            program: shipstern_core::Pubkey::new(ix.program_id.to_bytes()),
            accounts: ix
                .accounts
                .iter()
                .map(|m| shipstern_core::Pubkey::new(m.pubkey.to_bytes()))
                .collect(),
            data: ix.data.clone(),
            shared: Default::default(),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        }
    }

    fn pk(n: u8) -> shipstern_core::Pubkey { shipstern_core::Pubkey::new([n; 32]) }

    fn transfer_fee_ix(
        parsed: TokenExtensionProgram,
    ) -> crate::extensions::transfer_fee_instruction::Instruction {
        let Some(crate::instruction::Instruction::TransferFee(tf)) = parsed.instruction else {
            panic!("expected TransferFee");
        };

        tf.instruction
            .and_then(|i| i.instruction)
            .expect("empty transfer fee ix")
    }

    #[tokio::test]
    async fn harvest_withheld_tokens_lists_every_source_account() {
        use crate::extensions::transfer_fee_instruction::Instruction as Ix;

        let ix =
            spl_token_2022::extension::transfer_fee::instruction::harvest_withheld_tokens_to_mint(
                &spl_token_2022::ID,
                &key(1),
                &[&key(2), &key(3), &key(4)],
            )
            .unwrap();

        let parsed = InstructionParser.parse(&update_from(&ix)).await.unwrap();

        let Ix::HarvestWithheldTokensToMint(h) = transfer_fee_ix(parsed) else {
            panic!("expected HarvestWithheldTokensToMint");
        };

        assert_eq!(h.accounts.mint, pk(1));
        assert_eq!(h.accounts.source_accounts, vec![pk(2), pk(3), pk(4)]);
    }

    /// Every parser gets every instruction, so an SPL Token transfer must come back Filtered.
    #[tokio::test]
    async fn spl_token_instruction_is_filtered() {
        let ix = InstructionUpdate {
            program: shipstern_spl_token_parser::InstructionParser.program_id(),
            accounts: vec![shipstern_core::Pubkey::new([1; 32]); 3],
            data: [&[3][..], &1_000u64.to_le_bytes()].concat(),
            shared: Default::default(),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        };

        let res = InstructionParser.parse(&ix).await;

        assert!(matches!(res, Err(ParseError::Filtered)), "{res:?}");
    }
}
