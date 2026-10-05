use shipstern_core::{
    instruction::InstructionUpdate, ParseError, ParseResult, Parser, Prefilter, ProgramParser,
};
use shipstern_parser::{check_min_accounts_req, Result, ResultExt};
use solana_loader_v3_interface::instruction::UpgradeableLoaderInstruction;

use crate::Pubkey;

#[derive(Debug, Clone, Copy)]
pub struct InstructionParser;

impl Parser for InstructionParser {
    type Input = InstructionUpdate;
    type Output = crate::BpfLoaderProgram;

    fn id(&self) -> std::borrow::Cow<'static, str> { "bpf_loader::InstructionParser".into() }

    fn prefilter(&self) -> Prefilter {
        Prefilter::builder()
            .transaction_accounts_include([solana_sdk_ids::bpf_loader_upgradeable::ID])
            .build()
            .unwrap()
    }

    async fn parse(&self, ix_update: &InstructionUpdate) -> ParseResult<Self::Output> {
        if ix_update
            .program
            .equals_ref(solana_sdk_ids::bpf_loader_upgradeable::ID)
        {
            InstructionParser::parse_impl(ix_update).map_err(|e| ParseError::Other(e.into()))
        } else {
            Err(ParseError::Filtered)
        }
    }
}

impl ProgramParser for InstructionParser {
    #[inline]
    fn program_id(&self) -> shipstern_core::Pubkey {
        solana_sdk_ids::bpf_loader_upgradeable::ID.to_bytes().into()
    }
}

impl InstructionParser {
    pub fn parse_impl(ix: &InstructionUpdate) -> Result<crate::BpfLoaderProgram> {
        let ix_type: UpgradeableLoaderInstruction = bincode::deserialize(&ix.data)
            .parse_err("Error unpacking BPF loader instruction data")?;

        let accounts_len = ix.accounts.len();

        let ix_msg = match ix_type {
            UpgradeableLoaderInstruction::InitializeBuffer => {
                check_min_accounts_req(accounts_len, 2)?;

                crate::instruction::Instruction::InitializeBuffer(
                    crate::instruction::InitializeBuffer {
                        accounts: Some(crate::InitializeBufferAccounts {
                            buffer: Pubkey::new(ix.accounts[0].0),
                            authority: Pubkey::new(ix.accounts[1].0),
                        }),
                    },
                )
            },

            UpgradeableLoaderInstruction::Write { offset, bytes } => {
                check_min_accounts_req(accounts_len, 2)?;

                crate::instruction::Instruction::Write(crate::instruction::Write {
                    accounts: Some(crate::WriteAccounts {
                        buffer: Pubkey::new(ix.accounts[0].0),
                        authority: Pubkey::new(ix.accounts[1].0),
                    }),
                    args: Some(crate::WriteArgs { offset, bytes }),
                })
            },

            UpgradeableLoaderInstruction::DeployWithMaxDataLen { max_data_len } => {
                check_min_accounts_req(accounts_len, 8)?;

                crate::instruction::Instruction::Deploy(crate::instruction::Deploy {
                    accounts: Some(crate::DeployAccounts {
                        payer: Pubkey::new(ix.accounts[0].0),
                        program_data: Pubkey::new(ix.accounts[1].0),
                        program: Pubkey::new(ix.accounts[2].0),
                        buffer: Pubkey::new(ix.accounts[3].0),
                        rent: Pubkey::new(ix.accounts[4].0),
                        clock: Pubkey::new(ix.accounts[5].0),
                        system_program: Pubkey::new(ix.accounts[6].0),
                        authority: Pubkey::new(ix.accounts[7].0),
                    }),
                    args: Some(crate::DeployArgs {
                        max_data_len: max_data_len as u64,
                    }),
                })
            },

            UpgradeableLoaderInstruction::Upgrade => {
                check_min_accounts_req(accounts_len, 7)?;

                crate::instruction::Instruction::Upgrade(crate::instruction::Upgrade {
                    accounts: Some(crate::UpgradeAccounts {
                        program_data: Pubkey::new(ix.accounts[0].0),
                        program: Pubkey::new(ix.accounts[1].0),
                        buffer: Pubkey::new(ix.accounts[2].0),
                        spill: Pubkey::new(ix.accounts[3].0),
                        rent: Pubkey::new(ix.accounts[4].0),
                        clock: Pubkey::new(ix.accounts[5].0),
                        authority: Pubkey::new(ix.accounts[6].0),
                    }),
                })
            },

            UpgradeableLoaderInstruction::SetAuthority => {
                check_min_accounts_req(accounts_len, 2)?;

                crate::instruction::Instruction::SetAuthority(crate::instruction::SetAuthority {
                    accounts: Some(crate::SetAuthorityAccounts {
                        account: Pubkey::new(ix.accounts[0].0),
                        current_authority: Pubkey::new(ix.accounts[1].0),
                        new_authority: ix.accounts.get(2).map(|k| Pubkey::new(k.0)),
                    }),
                })
            },

            UpgradeableLoaderInstruction::Close => {
                check_min_accounts_req(accounts_len, 2)?;

                crate::instruction::Instruction::Close(crate::instruction::Close {
                    accounts: Some(crate::CloseAccounts {
                        close_target: Pubkey::new(ix.accounts[0].0),
                        recipient: Pubkey::new(ix.accounts[1].0),
                        authority: ix.accounts.get(2).map(|k| Pubkey::new(k.0)),
                        program: ix.accounts.get(3).map(|k| Pubkey::new(k.0)),
                    }),
                })
            },

            UpgradeableLoaderInstruction::ExtendProgram { additional_bytes } => {
                check_min_accounts_req(accounts_len, 2)?;

                crate::instruction::Instruction::ExtendProgram(crate::instruction::ExtendProgram {
                    accounts: Some(crate::ExtendProgramAccounts {
                        program_data: Pubkey::new(ix.accounts[0].0),
                        program: Pubkey::new(ix.accounts[1].0),
                        system_program: ix.accounts.get(2).map(|k| Pubkey::new(k.0)),
                        payer: ix.accounts.get(3).map(|k| Pubkey::new(k.0)),
                    }),
                    args: Some(crate::ExtendProgramArgs { additional_bytes }),
                })
            },

            UpgradeableLoaderInstruction::SetAuthorityChecked => {
                check_min_accounts_req(accounts_len, 3)?;

                crate::instruction::Instruction::SetAuthorityChecked(
                    crate::instruction::SetAuthorityChecked {
                        accounts: Some(crate::SetAuthorityCheckedAccounts {
                            account: Pubkey::new(ix.accounts[0].0),
                            current_authority: Pubkey::new(ix.accounts[1].0),
                            new_authority: Pubkey::new(ix.accounts[2].0),
                        }),
                    },
                )
            },
            // solana-loader-v3-interface 7 dropped Migrate and ExtendProgramChecked,
            // so the match is now exhaustive; unknown discriminants fail earlier in
            // `UpgradeableLoaderInstruction::try_from_slice`.
        };

        Ok(crate::BpfLoaderProgram {
            instruction: Some(ix_msg),
        })
    }
}
