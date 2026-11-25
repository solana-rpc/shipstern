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

#[cfg(test)]
mod tests {
    use solana_instruction::Instruction;
    use solana_loader_v3_interface::{get_program_data_address, instruction as builder};
    use solana_pubkey::Pubkey as SolanaPubkey;
    use solana_sdk_ids::{bpf_loader_upgradeable, system_program, sysvar};

    use super::InstructionParser;
    use crate::{instruction::Instruction as Ix, *};

    fn key(n: u8) -> SolanaPubkey { SolanaPubkey::new_from_array([n; 32]) }

    fn pk(key: SolanaPubkey) -> Pubkey { Pubkey::new(key.to_bytes()) }

    fn parse(ix: Instruction) -> Ix {
        assert_eq!(ix.program_id, bpf_loader_upgradeable::ID);
        let update = shipstern_core::instruction::InstructionUpdate {
            program: pk(ix.program_id),
            accounts: ix.accounts.iter().map(|meta| pk(meta.pubkey)).collect(),
            data: ix.data,
            shared: Default::default(),
            inner: vec![],
            path: shipstern_core::instruction::Path::new_single(0),
            log_range: 0..0,
        };

        InstructionParser::parse_impl(&update)
            .unwrap()
            .instruction
            .unwrap()
    }

    fn close(authority: Option<u8>, program: Option<u8>) -> Ix {
        Ix::Close(instruction::Close {
            accounts: Some(CloseAccounts {
                close_target: pk(key(1)),
                recipient: pk(key(2)),
                authority: authority.map(|n| pk(key(n))),
                program: program.map(|n| pk(key(n))),
            }),
        })
    }

    /// Each builder of solana-loader-v3-interface parses into the accounts it was given. The
    /// account order is the one the upgradeable loader reads.
    #[test]
    fn builder_instructions_parse_into_their_accounts() {
        let program_data = pk(get_program_data_address(&key(3)));
        let cases = [
            (
                builder::create_buffer(&key(9), &key(1), &key(2), 1, 10).unwrap()[1].clone(),
                Ix::InitializeBuffer(instruction::InitializeBuffer {
                    accounts: Some(InitializeBufferAccounts {
                        buffer: pk(key(1)),
                        authority: pk(key(2)),
                    }),
                }),
            ),
            (
                builder::write(&key(1), &key(2), 7, vec![1, 2]),
                Ix::Write(instruction::Write {
                    accounts: Some(WriteAccounts {
                        buffer: pk(key(1)),
                        authority: pk(key(2)),
                    }),
                    args: Some(WriteArgs {
                        offset: 7,
                        bytes: vec![1, 2],
                    }),
                }),
            ),
            (
                builder::deploy_with_max_program_len(&key(9), &key(3), &key(4), &key(2), 1, 100)
                    .unwrap()[1]
                    .clone(),
                Ix::Deploy(instruction::Deploy {
                    accounts: Some(DeployAccounts {
                        payer: pk(key(9)),
                        program_data,
                        program: pk(key(3)),
                        buffer: pk(key(4)),
                        rent: pk(sysvar::rent::ID),
                        clock: pk(sysvar::clock::ID),
                        system_program: pk(system_program::ID),
                        authority: pk(key(2)),
                    }),
                    args: Some(DeployArgs { max_data_len: 100 }),
                }),
            ),
            (
                builder::upgrade(&key(3), &key(4), &key(2), &key(5)),
                Ix::Upgrade(instruction::Upgrade {
                    accounts: Some(UpgradeAccounts {
                        program_data,
                        program: pk(key(3)),
                        buffer: pk(key(4)),
                        spill: pk(key(5)),
                        rent: pk(sysvar::rent::ID),
                        clock: pk(sysvar::clock::ID),
                        authority: pk(key(2)),
                    }),
                }),
            ),
            (
                builder::set_buffer_authority(&key(1), &key(2), &key(6)),
                Ix::SetAuthority(instruction::SetAuthority {
                    accounts: Some(SetAuthorityAccounts {
                        account: pk(key(1)),
                        current_authority: pk(key(2)),
                        new_authority: Some(pk(key(6))),
                    }),
                }),
            ),
            (
                builder::set_upgrade_authority(&key(3), &key(2), None),
                Ix::SetAuthority(instruction::SetAuthority {
                    accounts: Some(SetAuthorityAccounts {
                        account: program_data,
                        current_authority: pk(key(2)),
                        new_authority: None,
                    }),
                }),
            ),
            (
                builder::set_upgrade_authority_checked(&key(3), &key(2), &key(6)),
                Ix::SetAuthorityChecked(instruction::SetAuthorityChecked {
                    accounts: Some(SetAuthorityCheckedAccounts {
                        account: program_data,
                        current_authority: pk(key(2)),
                        new_authority: pk(key(6)),
                    }),
                }),
            ),
            (
                builder::close_any(&key(1), &key(2), None, None),
                close(None, None),
            ),
            (
                builder::close(&key(1), &key(2), &key(7)),
                close(Some(7), None),
            ),
            (
                builder::close_any(&key(1), &key(2), Some(&key(7)), Some(&key(3))),
                close(Some(7), Some(3)),
            ),
            // The loader reads accounts by position: without an authority, the program sits where
            // the authority goes, and the loader also takes it as the authority.
            (
                builder::close_any(&key(1), &key(2), None, Some(&key(3))),
                close(Some(3), None),
            ),
            (
                builder::extend_program(&key(3), None, 64),
                Ix::ExtendProgram(instruction::ExtendProgram {
                    accounts: Some(ExtendProgramAccounts {
                        program_data,
                        program: pk(key(3)),
                        system_program: None,
                        payer: None,
                    }),
                    args: Some(ExtendProgramArgs {
                        additional_bytes: 64,
                    }),
                }),
            ),
            (
                builder::extend_program(&key(3), Some(&key(9)), 64),
                Ix::ExtendProgram(instruction::ExtendProgram {
                    accounts: Some(ExtendProgramAccounts {
                        program_data,
                        program: pk(key(3)),
                        system_program: Some(pk(system_program::ID)),
                        payer: Some(pk(key(9))),
                    }),
                    args: Some(ExtendProgramArgs {
                        additional_bytes: 64,
                    }),
                }),
            ),
        ];

        for (ix, expected) in cases {
            assert_eq!(parse(ix), expected);
        }
    }
}
