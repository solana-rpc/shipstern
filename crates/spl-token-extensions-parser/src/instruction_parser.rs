use std::sync::Arc;

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
        CommonExtensionInstructions, ConfidentialMintBurnIx, ConfidentialTransferFeeIx,
        ConfidentialTransferIx, ExtensionWithCommonInstruction, PausableIx, PermissionedBurnIx,
        ScaledUiAmountIx, TokenGroupIx, TokenMetadataIx, TransferFeeIx,
    },
    instructions::{
        CreateNativeMintAccounts, InitializeMintCloseAuthorityAccounts,
        InitializeMintCloseAuthorityArgs, InitializeNonTransferableMintAccounts,
        InitializePermanentDelegateAccounts, InitializePermanentDelegateArgs, ReallocateAccounts,
        ReallocateArgs, SetAuthorityArgs, WithdrawExcessLamportsAccounts,
    },
    AuthorityType, BatchEntry, ExtensionInstructionParser, TokenExtensionProgram,
};

const BATCH_TAG: u8 = 255;

fn invalid_data(message: &'static str) -> Error { Error::new(message) }

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

                SplTokenInstruction::ScaledUiAmountExtension => {
                    let parsed = ScaledUiAmountIx::try_parse(ix)?;

                    Ok(envelope!(crate::instruction::Instruction::ScaledUiAmount(
                        crate::instruction::ScaledUiAmount {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::PausableExtension => {
                    let parsed = PausableIx::try_parse(ix)?;

                    Ok(envelope!(crate::instruction::Instruction::Pausable(
                        crate::instruction::Pausable {
                            instruction: Some(parsed),
                        },
                    )))
                },

                SplTokenInstruction::ConfidentialMintBurnExtension => {
                    let parsed = ConfidentialMintBurnIx::try_parse(ix)?;

                    Ok(envelope!(
                        crate::instruction::Instruction::ConfidentialMintBurn(
                            crate::instruction::ConfidentialMintBurn {
                                instruction: Some(parsed),
                            },
                        )
                    ))
                },

                SplTokenInstruction::PermissionedBurnExtension => {
                    let parsed = PermissionedBurnIx::try_parse(ix)?;

                    Ok(envelope!(
                        crate::instruction::Instruction::PermissionedBurn(
                            crate::instruction::PermissionedBurn {
                                instruction: Some(parsed),
                            },
                        )
                    ))
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

                SplTokenInstruction::Batch { .. } => Self::parse_batch(ix),

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

    /// Parses each inner instruction of a batch as a Token-2022 instruction, laid out as
    /// `[account count: u8][data length: u8][data]` per entry (`process_batch` in Token-2022).
    fn parse_batch(ix: &InstructionUpdate) -> Result<TokenExtensionProgram> {
        let mut cursor = 1;
        let mut account_offset = 0usize;
        let mut instructions = Vec::new();

        // Token-2022 rejects an empty batch.
        if ix.data.len() <= cursor {
            return Err(invalid_data("Empty Token-2022 batch"));
        }

        while cursor < ix.data.len() {
            let number_of_accounts_byte = ix.data[cursor];
            let number_of_accounts = usize::from(number_of_accounts_byte);
            cursor += 1;

            let instruction_data_len = usize::from(
                *ix.data
                    .get(cursor)
                    .ok_or_else(|| invalid_data("Missing Token-2022 batch instruction length"))?,
            );
            cursor += 1;

            let next_cursor = cursor + instruction_data_len;
            let instruction_data = ix
                .data
                .get(cursor..next_cursor)
                .ok_or_else(|| invalid_data("Truncated Token-2022 batch instruction data"))?;
            cursor = next_cursor;

            let account_end = account_offset + number_of_accounts;
            let accounts = ix
                .accounts
                .get(account_offset..account_end)
                .ok_or_else(|| invalid_data("Too few accounts for Token-2022 batch entry"))?;
            account_offset = account_end;

            // Token-2022 rejects a nested batch, so it stays unparsed.
            let parsed_instruction = if instruction_data.first() == Some(&BATCH_TAG) {
                None
            } else {
                let batch_index = instructions
                    .len()
                    .try_into()
                    .map_err(|_| invalid_data("Too many Token-2022 batch entries"))?;
                let inner_ix = InstructionUpdate {
                    program: ix.program,
                    accounts: accounts.to_vec(),
                    data: instruction_data.to_vec(),
                    shared: Arc::clone(&ix.shared),
                    inner: Vec::new(),
                    path: ix.path.push_clone(batch_index),
                    log_range: ix.log_range.clone(),
                };

                Self::parse_impl(&inner_ix).ok()
            };

            instructions.push(BatchEntry {
                number_of_accounts: u32::from(number_of_accounts_byte),
                accounts: accounts.to_vec(),
                data: instruction_data.to_vec(),
                instruction: parsed_instruction,
            });
        }

        Ok(TokenExtensionProgram {
            instruction: Some(crate::instruction::Instruction::Batch(
                crate::instruction::Batch {
                    instructions,
                    remaining_accounts: ix.accounts[account_offset..].to_vec(),
                },
            )),
        })
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

    #[tokio::test]
    async fn scaled_ui_amount_instructions_parse() {
        use crate::extensions::scaled_ui_amount_instruction::Instruction as Ix;

        let init = spl_token_2022::extension::scaled_ui_amount::instruction::initialize(
            &spl_token_2022::ID,
            &key(1),
            Some(key(2)),
            1.5,
        )
        .unwrap();
        let update = spl_token_2022::extension::scaled_ui_amount::instruction::update_multiplier(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &[&key(3)],
            2.5,
            1_700_000_000,
        )
        .unwrap();

        let Some(crate::instruction::Instruction::ScaledUiAmount(init)) = InstructionParser
            .parse(&update_from(&init))
            .await
            .unwrap()
            .instruction
        else {
            panic!("expected ScaledUiAmount");
        };
        let Some(Ix::Initialize(init)) = init.instruction.and_then(|i| i.instruction) else {
            panic!("expected Initialize");
        };

        assert_eq!(init.accounts.mint, pk(1));
        assert_eq!(init.args.authority, Some(pk(2)));
        assert_eq!(init.args.multiplier, 1.5);

        let Some(crate::instruction::Instruction::ScaledUiAmount(update)) = InstructionParser
            .parse(&update_from(&update))
            .await
            .unwrap()
            .instruction
        else {
            panic!("expected ScaledUiAmount");
        };
        let Some(Ix::UpdateMultiplier(update)) = update.instruction.and_then(|i| i.instruction)
        else {
            panic!("expected UpdateMultiplier");
        };

        assert_eq!(update.accounts.authority, pk(2));
        assert_eq!(update.accounts.multisig_signers, vec![pk(3)]);
        assert_eq!(update.args.multiplier, 2.5);
        assert_eq!(update.args.effective_timestamp, 1_700_000_000);
    }

    #[tokio::test]
    async fn pausable_instructions_parse() {
        use crate::extensions::pausable_instruction::Instruction as Ix;

        let init = spl_token_2022::extension::pausable::instruction::initialize(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
        )
        .unwrap();
        let pause = spl_token_2022::extension::pausable::instruction::pause(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &[&key(3)],
        )
        .unwrap();
        let resume = spl_token_2022::extension::pausable::instruction::resume(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &[],
        )
        .unwrap();

        let mut parsed = vec![];
        for ix in [&init, &pause, &resume] {
            let Some(crate::instruction::Instruction::Pausable(p)) = InstructionParser
                .parse(&update_from(ix))
                .await
                .unwrap()
                .instruction
            else {
                panic!("expected Pausable");
            };
            parsed.push(
                p.instruction
                    .and_then(|i| i.instruction)
                    .expect("empty pausable ix"),
            );
        }

        let [Ix::Initialize(init), Ix::Pause(pause), Ix::Resume(resume)] = &parsed[..] else {
            panic!("unexpected variants");
        };

        assert_eq!(init.accounts.mint, pk(1));
        assert_eq!(init.args.authority, pk(2));
        assert_eq!(pause.accounts.authority, pk(2));
        assert_eq!(pause.accounts.multisig_signers, vec![pk(3)]);
        assert!(resume.accounts.multisig_signers.is_empty());
    }

    async fn parse_confidential_mint_burn(
        ix: &solana_instruction::Instruction,
    ) -> crate::extensions::confidential_mint_burn_instruction::Instruction {
        let Some(crate::instruction::Instruction::ConfidentialMintBurn(c)) = InstructionParser
            .parse(&update_from(ix))
            .await
            .unwrap()
            .instruction
        else {
            panic!("expected ConfidentialMintBurn");
        };

        c.instruction
            .and_then(|i| i.instruction)
            .expect("empty confidential mint burn ix")
    }

    #[tokio::test]
    async fn confidential_mint_burn_supply_instructions_parse() {
        use spl_token_2022::extension::confidential_mint_burn::instruction::{
            apply_pending_burn, initialize_mint, rotate_supply_elgamal_pubkey,
            update_decryptable_supply,
        };
        use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

        use crate::extensions::confidential_mint_burn_instruction::Instruction as Ix;

        let init = initialize_mint(
            &spl_token_2022::ID,
            &key(1),
            &bytemuck::cast([5u8; 32]),
            &bytemuck::pod_read_unaligned(&[6u8; 36]),
        )
        .unwrap();
        let Ix::InitializeMint(init) = parse_confidential_mint_burn(&init).await else {
            panic!("expected InitializeMint");
        };
        assert_eq!(init.accounts.mint, pk(1));
        assert_eq!(init.args.supply_elgamal_pubkey, vec![5u8; 32]);
        assert_eq!(init.args.decryptable_supply, vec![6u8; 36]);

        // Proof in the same transaction: account 1 is the instructions sysvar.
        let rotate = rotate_supply_elgamal_pubkey(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &[&key(3), &key(4)],
            &bytemuck::cast([7u8; 32]),
            ProofLocation::InstructionOffset(
                std::num::NonZeroI8::new(1).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
        )
        .unwrap();
        let Ix::RotateSupplyElGamalPubkey(rotate) = parse_confidential_mint_burn(&rotate[0]).await
        else {
            panic!("expected RotateSupplyElGamalPubkey");
        };
        assert_eq!(rotate.accounts.mint, pk(1));
        assert!(rotate.accounts.instructions_sysvar.is_some());
        assert_eq!(rotate.accounts.equality_context, None);
        assert_eq!(rotate.accounts.authority, pk(2));
        assert_eq!(rotate.accounts.multisig_signers, vec![pk(3), pk(4)]);
        assert_eq!(rotate.args.new_supply_elgamal_pubkey, vec![7u8; 32]);
        assert_eq!(rotate.args.proof_instruction_offset, 1);

        // Pre-verified proof: account 1 is the context state account.
        let rotate = rotate_supply_elgamal_pubkey(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &[],
            &bytemuck::cast([7u8; 32]),
            ProofLocation::ContextStateAccount(&key(9)),
        )
        .unwrap();
        let Ix::RotateSupplyElGamalPubkey(rotate) = parse_confidential_mint_burn(&rotate[0]).await
        else {
            panic!("expected RotateSupplyElGamalPubkey");
        };
        assert_eq!(rotate.accounts.instructions_sysvar, None);
        assert_eq!(rotate.accounts.equality_context, Some(pk(9)));
        assert_eq!(rotate.accounts.authority, pk(2));
        assert!(rotate.accounts.multisig_signers.is_empty());
        assert_eq!(rotate.args.proof_instruction_offset, 0);

        let update = update_decryptable_supply(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &[&key(3)],
            &bytemuck::pod_read_unaligned(&[8u8; 36]),
        )
        .unwrap();
        let Ix::UpdateDecryptableSupply(update) = parse_confidential_mint_burn(&update).await
        else {
            panic!("expected UpdateDecryptableSupply");
        };
        assert_eq!(update.accounts.mint, pk(1));
        assert_eq!(update.accounts.authority, pk(2));
        assert_eq!(update.accounts.multisig_signers, vec![pk(3)]);
        assert_eq!(update.args.new_decryptable_supply, vec![8u8; 36]);

        let apply = apply_pending_burn(&spl_token_2022::ID, &key(1), &key(2), &[&key(3)]).unwrap();
        let Ix::ApplyPendingBurn(apply) = parse_confidential_mint_burn(&apply).await else {
            panic!("expected ApplyPendingBurn");
        };
        assert_eq!(apply.accounts.mint, pk(1));
        assert_eq!(apply.accounts.authority, pk(2));
        assert_eq!(apply.accounts.multisig_signers, vec![pk(3)]);
    }

    #[tokio::test]
    async fn confidential_mint_reads_proof_accounts_from_offsets() {
        use std::num::NonZeroI8;

        use spl_token_2022::extension::confidential_mint_burn::instruction::inner_confidential_mint;
        use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

        use crate::extensions::confidential_mint_burn_instruction::Instruction as Ix;

        // All proofs pre-verified: three context accounts, no sysvar.
        let mint = inner_confidential_mint(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(3),
            &[&key(4), &key(5)],
            ProofLocation::ContextStateAccount(&key(10)),
            ProofLocation::ContextStateAccount(&key(11)),
            ProofLocation::ContextStateAccount(&key(12)),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
        )
        .unwrap();
        let Ix::Mint(mint) = parse_confidential_mint_burn(&mint).await else {
            panic!("expected Mint");
        };
        assert_eq!(mint.accounts.token_account, pk(1));
        assert_eq!(mint.accounts.mint, pk(2));
        assert_eq!(mint.accounts.instructions_sysvar, None);
        assert_eq!(mint.accounts.equality_context, Some(pk(10)));
        assert_eq!(mint.accounts.ciphertext_validity_context, Some(pk(11)));
        assert_eq!(mint.accounts.range_context, Some(pk(12)));
        assert_eq!(mint.accounts.authority, pk(3));
        assert_eq!(mint.accounts.multisig_signers, vec![pk(4), pk(5)]);
        assert_eq!(mint.args.new_decryptable_supply, vec![7u8; 36]);
        assert_eq!(mint.args.mint_amount_auditor_ciphertext_lo, vec![5u8; 64]);
        assert_eq!(mint.args.mint_amount_auditor_ciphertext_hi, vec![6u8; 64]);

        // Mixed: equality and range in the transaction, validity pre-verified.
        let mint = inner_confidential_mint(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(3),
            &[],
            ProofLocation::InstructionOffset(
                NonZeroI8::new(1).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
            ProofLocation::ContextStateAccount(&key(11)),
            ProofLocation::InstructionOffset(
                NonZeroI8::new(2).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
        )
        .unwrap();
        let Ix::Mint(mint) = parse_confidential_mint_burn(&mint).await else {
            panic!("expected Mint");
        };
        assert!(mint.accounts.instructions_sysvar.is_some());
        assert_eq!(mint.accounts.equality_context, None);
        assert_eq!(mint.accounts.ciphertext_validity_context, Some(pk(11)));
        assert_eq!(mint.accounts.range_context, None);
        assert_eq!(mint.accounts.authority, pk(3));
        assert!(mint.accounts.multisig_signers.is_empty());
        assert_eq!(mint.args.equality_proof_instruction_offset, 1);
        assert_eq!(mint.args.ciphertext_validity_proof_instruction_offset, 0);
        assert_eq!(mint.args.range_proof_instruction_offset, 2);
    }

    #[tokio::test]
    async fn confidential_burn_reads_proof_accounts_from_offsets() {
        use std::num::NonZeroI8;

        use spl_token_2022::extension::confidential_mint_burn::instruction::inner_confidential_burn;
        use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

        use crate::extensions::confidential_mint_burn_instruction::Instruction as Ix;

        // All proofs in the transaction: only the sysvar sits before the owner.
        let burn = inner_confidential_burn(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(3),
            &[&key(4)],
            ProofLocation::InstructionOffset(
                NonZeroI8::new(1).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
            ProofLocation::InstructionOffset(
                NonZeroI8::new(2).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
            ProofLocation::InstructionOffset(
                NonZeroI8::new(3).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
        )
        .unwrap();
        let Ix::Burn(burn) = parse_confidential_mint_burn(&burn).await else {
            panic!("expected Burn");
        };
        assert_eq!(burn.accounts.token_account, pk(1));
        assert_eq!(burn.accounts.mint, pk(2));
        assert!(burn.accounts.instructions_sysvar.is_some());
        assert_eq!(burn.accounts.equality_context, None);
        assert_eq!(burn.accounts.ciphertext_validity_context, None);
        assert_eq!(burn.accounts.range_context, None);
        assert_eq!(burn.accounts.owner, pk(3));
        assert_eq!(burn.accounts.multisig_signers, vec![pk(4)]);
        assert_eq!(burn.args.new_decryptable_available_balance, vec![7u8; 36]);
        assert_eq!(burn.args.burn_amount_auditor_ciphertext_lo, vec![5u8; 64]);
        assert_eq!(burn.args.burn_amount_auditor_ciphertext_hi, vec![6u8; 64]);
        assert_eq!(burn.args.range_proof_instruction_offset, 3);

        // Mixed: equality pre-verified, validity in the transaction, range pre-verified.
        let burn = inner_confidential_burn(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(3),
            &[],
            ProofLocation::ContextStateAccount(&key(10)),
            ProofLocation::InstructionOffset(
                NonZeroI8::new(1).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
            ProofLocation::ContextStateAccount(&key(12)),
        )
        .unwrap();
        let Ix::Burn(burn) = parse_confidential_mint_burn(&burn).await else {
            panic!("expected Burn");
        };
        assert!(burn.accounts.instructions_sysvar.is_some());
        assert_eq!(burn.accounts.equality_context, Some(pk(10)));
        assert_eq!(burn.accounts.ciphertext_validity_context, None);
        assert_eq!(burn.accounts.range_context, Some(pk(12)));
        assert_eq!(burn.accounts.owner, pk(3));
        assert!(burn.accounts.multisig_signers.is_empty());
    }

    #[tokio::test]
    async fn confidential_burn_without_the_owner_is_rejected() {
        use spl_token_2022::extension::confidential_mint_burn::instruction::inner_confidential_burn;
        use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

        let mut burn = inner_confidential_burn(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(3),
            &[],
            ProofLocation::ContextStateAccount(&key(10)),
            ProofLocation::ContextStateAccount(&key(11)),
            ProofLocation::ContextStateAccount(&key(12)),
        )
        .unwrap();
        burn.accounts.pop();

        assert!(InstructionParser.parse(&update_from(&burn)).await.is_err());
    }

    async fn parse_permissioned_burn(
        ix: &solana_instruction::Instruction,
    ) -> crate::extensions::permissioned_burn_instruction::Instruction {
        let Some(crate::instruction::Instruction::PermissionedBurn(p)) = InstructionParser
            .parse(&update_from(ix))
            .await
            .unwrap()
            .instruction
        else {
            panic!("expected PermissionedBurn");
        };

        p.instruction
            .and_then(|i| i.instruction)
            .expect("empty permissioned burn ix")
    }

    #[tokio::test]
    async fn permissioned_burn_instructions_parse() {
        use spl_token_2022::extension::permissioned_burn::instruction::{
            burn, burn_checked, initialize,
        };

        use crate::extensions::permissioned_burn_instruction::Instruction as Ix;

        let init = initialize(&spl_token_2022::ID, &key(1), &key(2)).unwrap();
        let Ix::Initialize(init) = parse_permissioned_burn(&init).await else {
            panic!("expected Initialize");
        };
        assert_eq!(init.accounts.mint, pk(1));
        assert_eq!(init.args.authority, pk(2));

        let b = burn(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &key(4),
            &[&key(5)],
            42,
        )
        .unwrap();
        let Ix::Burn(b) = parse_permissioned_burn(&b).await else {
            panic!("expected Burn");
        };
        assert_eq!(b.accounts.account, pk(1));
        assert_eq!(b.accounts.mint, pk(2));
        assert_eq!(b.accounts.permissioned_burn_authority, pk(3));
        assert_eq!(b.accounts.owner, pk(4));
        assert_eq!(b.accounts.multisig_signers, vec![pk(5)]);
        assert_eq!(b.args.amount, 42);

        let b = burn_checked(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &key(4),
            &[],
            7,
            9,
        )
        .unwrap();
        let Ix::BurnChecked(b) = parse_permissioned_burn(&b).await else {
            panic!("expected BurnChecked");
        };
        assert_eq!(b.accounts.account, pk(1));
        assert_eq!(b.accounts.mint, pk(2));
        assert_eq!(b.accounts.permissioned_burn_authority, pk(3));
        assert_eq!(b.accounts.owner, pk(4));
        assert!(b.accounts.multisig_signers.is_empty());
        assert_eq!(b.args.amount, 7);
        assert_eq!(b.args.decimals, 9);
    }

    #[tokio::test]
    async fn permissioned_confidential_burn_reads_proof_accounts_from_offsets() {
        use std::num::NonZeroI8;

        use spl_token_2022::extension::permissioned_burn::instruction::confidential_burn_with_split_proofs;
        use spl_token_confidential_transfer_proof_extraction::instruction::ProofLocation;

        use crate::extensions::permissioned_burn_instruction::Instruction as Ix;

        // All proofs pre-verified: three context accounts, no sysvar.
        let burn = confidential_burn_with_split_proofs(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(4),
            &[&key(5)],
            ProofLocation::ContextStateAccount(&key(10)),
            ProofLocation::ContextStateAccount(&key(11)),
            ProofLocation::ContextStateAccount(&key(12)),
        )
        .unwrap();
        let Ix::ConfidentialBurn(burn) = parse_permissioned_burn(&burn[0]).await else {
            panic!("expected ConfidentialBurn");
        };
        assert_eq!(burn.accounts.token_account, pk(1));
        assert_eq!(burn.accounts.mint, pk(2));
        assert_eq!(burn.accounts.instructions_sysvar, None);
        assert_eq!(burn.accounts.equality_context, Some(pk(10)));
        assert_eq!(burn.accounts.ciphertext_validity_context, Some(pk(11)));
        assert_eq!(burn.accounts.range_context, Some(pk(12)));
        assert_eq!(burn.accounts.permissioned_burn_authority, pk(3));
        assert_eq!(burn.accounts.owner, pk(4));
        assert_eq!(burn.accounts.multisig_signers, vec![pk(5)]);
        assert_eq!(burn.args.new_decryptable_available_balance, vec![7u8; 36]);
        assert_eq!(burn.args.burn_amount_auditor_ciphertext_lo, vec![5u8; 64]);
        assert_eq!(burn.args.burn_amount_auditor_ciphertext_hi, vec![6u8; 64]);

        // Mixed: equality and range in the transaction, validity pre-verified.
        let burn = confidential_burn_with_split_proofs(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(4),
            &[],
            ProofLocation::InstructionOffset(
                NonZeroI8::new(1).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
            ProofLocation::ContextStateAccount(&key(11)),
            ProofLocation::InstructionOffset(
                NonZeroI8::new(2).unwrap(),
                &bytemuck::Zeroable::zeroed(),
            ),
        )
        .unwrap();
        let Ix::ConfidentialBurn(burn) = parse_permissioned_burn(&burn[0]).await else {
            panic!("expected ConfidentialBurn");
        };
        assert!(burn.accounts.instructions_sysvar.is_some());
        assert_eq!(burn.accounts.equality_context, None);
        assert_eq!(burn.accounts.ciphertext_validity_context, Some(pk(11)));
        assert_eq!(burn.accounts.range_context, None);
        assert_eq!(burn.accounts.permissioned_burn_authority, pk(3));
        assert_eq!(burn.accounts.owner, pk(4));
        assert!(burn.accounts.multisig_signers.is_empty());
        assert_eq!(burn.args.equality_proof_instruction_offset, 1);
        assert_eq!(burn.args.range_proof_instruction_offset, 2);

        // Without the owner the account list is too short.
        let mut burn = confidential_burn_with_split_proofs(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &bytemuck::pod_read_unaligned(&[7u8; 36]),
            &bytemuck::cast([5u8; 64]),
            &bytemuck::cast([6u8; 64]),
            &key(4),
            &[],
            ProofLocation::ContextStateAccount(&key(10)),
            ProofLocation::ContextStateAccount(&key(11)),
            ProofLocation::ContextStateAccount(&key(12)),
        )
        .unwrap();
        burn[0].accounts.pop();
        assert!(InstructionParser
            .parse(&update_from(&burn[0]))
            .await
            .is_err());
    }

    /// The newest oneof variant must survive an encode and decode round trip.
    #[cfg(feature = "proto")]
    #[test]
    fn permissioned_burn_round_trips_through_protobuf() {
        use prost::Message;

        let ix = TokenExtensionProgram {
            instruction: Some(crate::instruction::Instruction::PermissionedBurn(
                crate::instruction::PermissionedBurn { instruction: None },
            )),
        };

        let decoded = TokenExtensionProgram::decode(ix.encode_to_vec().as_slice()).unwrap();

        assert_eq!(decoded, ix);
    }

    /// A batch holds parsed inner instructions, so its tag and the nested message must both
    /// decode.
    #[cfg(feature = "proto")]
    #[test]
    fn batch_round_trips_through_protobuf() {
        use prost::Message;

        let inner = TokenExtensionProgram {
            instruction: Some(crate::instruction::Instruction::PermissionedBurn(
                crate::instruction::PermissionedBurn { instruction: None },
            )),
        };
        let ix = TokenExtensionProgram {
            instruction: Some(crate::instruction::Instruction::Batch(
                crate::instruction::Batch {
                    instructions: vec![crate::BatchEntry {
                        number_of_accounts: 1,
                        accounts: vec![crate::Pubkey::new([7; 32])],
                        data: vec![46, 0],
                        instruction: Some(inner),
                    }],
                    remaining_accounts: vec![],
                },
            )),
        };

        let decoded = TokenExtensionProgram::decode(ix.encode_to_vec().as_slice()).unwrap();

        assert_eq!(decoded, ix);
    }

    /// Packs a Token-2022 batch like `process_batch` expects: per entry, the account count,
    /// the data length and the data. The accounts are the entries' accounts, concatenated.
    fn batch_of(ixs: &[&solana_instruction::Instruction]) -> solana_instruction::Instruction {
        let mut data = vec![255];
        let mut accounts = Vec::new();

        for ix in ixs {
            data.push(u8::try_from(ix.accounts.len()).unwrap());
            data.push(u8::try_from(ix.data.len()).unwrap());
            data.extend_from_slice(&ix.data);
            accounts.extend(ix.accounts.iter().cloned());
        }

        solana_instruction::Instruction {
            program_id: spl_token_2022::ID,
            accounts,
            data,
        }
    }

    async fn parse_batch(ix: &solana_instruction::Instruction) -> crate::instruction::Batch {
        let Some(crate::instruction::Instruction::Batch(batch)) = InstructionParser
            .parse(&update_from(ix))
            .await
            .unwrap()
            .instruction
        else {
            panic!("expected Batch");
        };

        batch
    }

    #[tokio::test]
    async fn batch_parses_inner_instructions_with_the_token_2022_parser() {
        use shipstern_spl_token_parser::instruction::Instruction as TokenIx;
        use spl_token_2022::extension::transfer_fee::instruction::transfer_checked_with_fee;

        use crate::extensions::transfer_fee_instruction::Instruction as FeeIx;

        let with_fee = transfer_checked_with_fee(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &key(4),
            &[],
            100,
            6,
            3,
        )
        .unwrap();
        let transfer = spl_token_2022::instruction::transfer_checked(
            &spl_token_2022::ID,
            &key(5),
            &key(6),
            &key(7),
            &key(8),
            &[],
            9,
            6,
        )
        .unwrap();

        let mut ix = batch_of(&[&with_fee, &transfer]);
        ix.accounts
            .push(solana_instruction::AccountMeta::new_readonly(key(9), false));

        let batch = parse_batch(&ix).await;
        assert_eq!(batch.instructions.len(), 2);
        assert_eq!(batch.remaining_accounts, vec![pk(9)]);

        let entry = &batch.instructions[0];
        assert_eq!(entry.number_of_accounts, 4);
        assert_eq!(entry.accounts, vec![pk(1), pk(2), pk(3), pk(4)]);
        assert_eq!(entry.data, with_fee.data);
        let Some(crate::instruction::Instruction::TransferFee(fee)) = entry
            .instruction
            .as_ref()
            .and_then(|i| i.instruction.clone())
        else {
            panic!("expected TransferFee");
        };
        let Some(FeeIx::TransferCheckedWithFee(fee)) = fee.instruction.and_then(|i| i.instruction)
        else {
            panic!("expected TransferCheckedWithFee");
        };
        assert_eq!(fee.accounts.source, pk(1));
        assert_eq!(fee.accounts.owner, pk(4));
        assert_eq!(fee.args.amount, 100);
        assert_eq!(fee.args.fee_amount, 3);
        assert_eq!(fee.args.decimals, 6);

        let entry = &batch.instructions[1];
        assert_eq!(entry.number_of_accounts, 4);
        let Some(crate::instruction::Instruction::TokenProgram(token)) = entry
            .instruction
            .as_ref()
            .and_then(|i| i.instruction.clone())
        else {
            panic!("expected TokenProgram");
        };
        let Some(TokenIx::TransferChecked(transfer)) =
            token.instruction.and_then(|i| i.instruction)
        else {
            panic!("expected TransferChecked");
        };
        assert_eq!(transfer.accounts.source, pk(5));
        assert_eq!(transfer.accounts.mint, pk(6));
        assert_eq!(transfer.accounts.destination, pk(7));
        assert_eq!(transfer.args.amount, 9);
    }

    #[tokio::test]
    async fn batch_leaves_nested_batch_and_unknown_entries_unparsed() {
        let transfer = spl_token_2022::instruction::transfer_checked(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &key(4),
            &[],
            1,
            6,
        )
        .unwrap();
        let inner = batch_of(&[&transfer]);
        let mut garbage = transfer.clone();
        garbage.data = vec![200, 1];

        let batch = parse_batch(&batch_of(&[&inner, &garbage, &transfer])).await;

        assert_eq!(batch.instructions.len(), 3);
        assert_eq!(batch.instructions[0].data, inner.data);
        assert_eq!(batch.instructions[0].instruction, None);
        assert_eq!(batch.instructions[1].instruction, None);
        assert_eq!(batch.instructions[1].data, vec![200, 1]);
        assert!(batch.instructions[2].instruction.is_some());
    }

    #[tokio::test]
    async fn malformed_batches_are_errors() {
        let transfer = spl_token_2022::instruction::transfer_checked(
            &spl_token_2022::ID,
            &key(1),
            &key(2),
            &key(3),
            &key(4),
            &[],
            1,
            6,
        )
        .unwrap();
        let good = batch_of(&[&transfer]);

        // Token-2022 rejects an empty batch.
        let mut empty = good.clone();
        empty.data = vec![255];
        empty.accounts.clear();
        assert!(InstructionParser.parse(&update_from(&empty)).await.is_err());

        // Header cut after the account count.
        let mut ix = good.clone();
        ix.data = vec![255, 3];
        assert!(InstructionParser.parse(&update_from(&ix)).await.is_err());

        // Data shorter than the declared length.
        let mut ix = good.clone();
        ix.data.pop();
        assert!(InstructionParser.parse(&update_from(&ix)).await.is_err());

        // Fewer accounts than the entry declares.
        let mut ix = good.clone();
        ix.accounts.pop();
        assert!(InstructionParser.parse(&update_from(&ix)).await.is_err());
    }
}
