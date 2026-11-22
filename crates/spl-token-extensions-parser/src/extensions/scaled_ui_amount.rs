use bytemuck::try_pod_read_unaligned;
use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Error, Result};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::scaled_ui_amount::instruction::{
    InitializeInstructionData, ScaledUiAmountMintInstruction, UpdateMultiplierInstructionData,
};

use super::extension::{decode_extension_ix_type, ExtensionInstructionParser};
use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ScaledUiAmountInitializeAccounts {
    pub mint: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ScaledUiAmountInitializeArgs {
    pub authority: Option<Pubkey>,
    pub multiplier: f64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateMultiplierAccounts {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateMultiplierArgs {
    pub multiplier: f64,
    pub effective_timestamp: i64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ScaledUiAmountIx {
    #[hint(oneof = "scaled_ui_amount_instruction::Instruction", tags = "1, 2")]
    pub instruction: Option<scaled_ui_amount_instruction::Instruction>,
}

pub mod scaled_ui_amount_instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Initialize {
        pub accounts: super::ScaledUiAmountInitializeAccounts,
        pub args: super::ScaledUiAmountInitializeArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct UpdateMultiplier {
        pub accounts: super::UpdateMultiplierAccounts,
        pub args: super::UpdateMultiplierArgs,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        Initialize(Initialize),
        UpdateMultiplier(UpdateMultiplier),
    }
}

impl ExtensionInstructionParser for ScaledUiAmountIx {
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();
        let ix_type: ScaledUiAmountMintInstruction = decode_extension_ix_type(&ix.data[1..])?;
        let data = &ix.data[2..];

        use scaled_ui_amount_instruction as oneof;

        let ix_msg = match ix_type {
            ScaledUiAmountMintInstruction::Initialize => {
                check_min_accounts_req(accounts_len, 1)?;

                let args: InitializeInstructionData = try_pod_read_unaligned(data)
                    .map_err(|_| Error::new("Invalid scaled ui amount initialize data"))?;

                let authority = args.authority.get();

                oneof::Instruction::Initialize(oneof::Initialize {
                    accounts: ScaledUiAmountInitializeAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                    },
                    args: ScaledUiAmountInitializeArgs {
                        authority: authority.map(|p| crate::Pubkey::new(p.to_bytes())),
                        multiplier: args.multiplier.into(),
                    },
                })
            },

            ScaledUiAmountMintInstruction::UpdateMultiplier => {
                check_min_accounts_req(accounts_len, 2)?;

                let args: UpdateMultiplierInstructionData = try_pod_read_unaligned(data)
                    .map_err(|_| Error::new("Invalid scaled ui amount update data"))?;

                oneof::Instruction::UpdateMultiplier(oneof::UpdateMultiplier {
                    accounts: UpdateMultiplierAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                        authority: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                    args: UpdateMultiplierArgs {
                        multiplier: args.multiplier.into(),
                        effective_timestamp: args.effective_timestamp.into(),
                    },
                })
            },
        };

        Ok(ScaledUiAmountIx {
            instruction: Some(ix_msg),
        })
    }
}
