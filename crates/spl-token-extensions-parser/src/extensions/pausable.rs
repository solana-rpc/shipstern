use bytemuck::try_pod_read_unaligned;
use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Error, Result};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::pausable::instruction::{
    InitializeInstructionData, PausableInstruction,
};

use super::extension::{decode_extension_ix_type, ExtensionInstructionParser};
use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PausableInitializeAccounts {
    pub mint: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PausableInitializeArgs {
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PauseAccounts {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ResumeAccounts {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PausableIx {
    #[hint(oneof = "pausable_instruction::Instruction", tags = "1, 2, 3")]
    pub instruction: Option<pausable_instruction::Instruction>,
}

pub mod pausable_instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Initialize {
        pub accounts: super::PausableInitializeAccounts,
        pub args: super::PausableInitializeArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Pause {
        pub accounts: super::PauseAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Resume {
        pub accounts: super::ResumeAccounts,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        Initialize(Initialize),
        Pause(Pause),
        Resume(Resume),
    }
}

impl ExtensionInstructionParser for PausableIx {
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();
        let ix_type: PausableInstruction = decode_extension_ix_type(&ix.data[1..])?;

        use pausable_instruction as oneof;

        let signers = |from: usize| -> Vec<crate::Pubkey> {
            ix.accounts[from..]
                .iter()
                .map(|a| crate::Pubkey::new(a.0))
                .collect()
        };

        let ix_msg = match ix_type {
            PausableInstruction::Initialize => {
                check_min_accounts_req(accounts_len, 1)?;

                let args: InitializeInstructionData = try_pod_read_unaligned(&ix.data[2..])
                    .map_err(|_| Error::new("Invalid pausable initialize data"))?;

                oneof::Instruction::Initialize(oneof::Initialize {
                    accounts: PausableInitializeAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                    },
                    args: PausableInitializeArgs {
                        authority: crate::Pubkey::new(args.authority.to_bytes()),
                    },
                })
            },

            PausableInstruction::Pause => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::Pause(oneof::Pause {
                    accounts: PauseAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                        authority: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: signers(2),
                    },
                })
            },

            PausableInstruction::Resume => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::Resume(oneof::Resume {
                    accounts: ResumeAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                        authority: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: signers(2),
                    },
                })
            },
        };

        Ok(PausableIx {
            instruction: Some(ix_msg),
        })
    }
}
