use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Result};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::{
    confidential_mint_burn::instruction::BurnInstructionData as ConfidentialBurnInstructionData,
    permissioned_burn::instruction::{
        BurnCheckedInstructionData, BurnInstructionData, InitializeInstructionData,
        PermissionedBurnInstruction,
    },
};

use super::{
    confidential_mint_burn::{burn_args, burn_offsets, decode_data, read_mint_burn_proof_accounts},
    extension::{decode_extension_ix_type, ExtensionInstructionParser},
    ConfidentialBurnArgs,
};
use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnInitializeAccounts {
    pub mint: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnInitializeArgs {
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub permissioned_burn_authority: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnArgs {
    pub amount: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnCheckedAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub permissioned_burn_authority: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnCheckedArgs {
    pub amount: u64,
    pub decimals: u32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedConfidentialBurnAccounts {
    pub token_account: Pubkey,
    pub mint: Pubkey,
    pub instructions_sysvar: Option<Pubkey>,
    pub equality_context: Option<Pubkey>,
    pub ciphertext_validity_context: Option<Pubkey>,
    pub range_context: Option<Pubkey>,
    pub permissioned_burn_authority: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct PermissionedBurnIx {
    #[hint(
        oneof = "permissioned_burn_instruction::Instruction",
        tags = "1, 2, 3, 4"
    )]
    pub instruction: Option<permissioned_burn_instruction::Instruction>,
}

pub mod permissioned_burn_instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Initialize {
        pub accounts: super::PermissionedBurnInitializeAccounts,
        pub args: super::PermissionedBurnInitializeArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Burn {
        pub accounts: super::PermissionedBurnAccounts,
        pub args: super::PermissionedBurnArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct BurnChecked {
        pub accounts: super::PermissionedBurnCheckedAccounts,
        pub args: super::PermissionedBurnCheckedArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ConfidentialBurn {
        pub accounts: super::PermissionedConfidentialBurnAccounts,
        pub args: super::ConfidentialBurnArgs,
    }

    // `ConfidentialBurn` carries nine accounts; the other variants carry a few.
    #[allow(clippy::large_enum_variant)]
    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        Initialize(Initialize),
        Burn(Burn),
        BurnChecked(BurnChecked),
        ConfidentialBurn(ConfidentialBurn),
    }
}

impl ExtensionInstructionParser for PermissionedBurnIx {
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();
        let ix_type: PermissionedBurnInstruction = decode_extension_ix_type(&ix.data[1..])?;

        use permissioned_burn_instruction as oneof;

        let key = |i: usize| Pubkey::new(ix.accounts[i].0);
        let signers = |from: usize| -> Vec<Pubkey> {
            ix.accounts[from..]
                .iter()
                .map(|a| Pubkey::new(a.0))
                .collect()
        };

        let ix_msg = match ix_type {
            PermissionedBurnInstruction::Initialize => {
                check_min_accounts_req(accounts_len, 1)?;

                let data: InitializeInstructionData =
                    decode_data(ix, "permissioned burn initialize")?;

                oneof::Instruction::Initialize(oneof::Initialize {
                    accounts: PermissionedBurnInitializeAccounts { mint: key(0) },
                    args: PermissionedBurnInitializeArgs {
                        authority: Pubkey::new(data.authority.to_bytes()),
                    },
                })
            },

            // [account, mint, permissioned burn authority, owner, signers..]
            PermissionedBurnInstruction::Burn => {
                check_min_accounts_req(accounts_len, 4)?;

                let data: BurnInstructionData = decode_data(ix, "permissioned burn")?;

                oneof::Instruction::Burn(oneof::Burn {
                    accounts: PermissionedBurnAccounts {
                        account: key(0),
                        mint: key(1),
                        permissioned_burn_authority: key(2),
                        owner: key(3),
                        multisig_signers: signers(4),
                    },
                    args: PermissionedBurnArgs {
                        amount: data.amount.into(),
                    },
                })
            },

            PermissionedBurnInstruction::BurnChecked => {
                check_min_accounts_req(accounts_len, 4)?;

                let data: BurnCheckedInstructionData =
                    decode_data(ix, "permissioned burn checked")?;

                oneof::Instruction::BurnChecked(oneof::BurnChecked {
                    accounts: PermissionedBurnCheckedAccounts {
                        account: key(0),
                        mint: key(1),
                        permissioned_burn_authority: key(2),
                        owner: key(3),
                        multisig_signers: signers(4),
                    },
                    args: PermissionedBurnCheckedArgs {
                        amount: data.amount.into(),
                        decimals: data.decimals.into(),
                    },
                })
            },

            // Same as the confidential mint burn `Burn`, with the permissioned burn authority
            // between the proof accounts and the owner.
            PermissionedBurnInstruction::ConfidentialBurn => {
                check_min_accounts_req(accounts_len, 2)?;

                let data: ConfidentialBurnInstructionData =
                    decode_data(ix, "permissioned confidential burn")?;
                let proofs = read_mint_burn_proof_accounts(ix, 2, burn_offsets(&data))?;
                check_min_accounts_req(accounts_len, proofs.next + 2)?;

                oneof::Instruction::ConfidentialBurn(oneof::ConfidentialBurn {
                    accounts: PermissionedConfidentialBurnAccounts {
                        token_account: key(0),
                        mint: key(1),
                        instructions_sysvar: proofs.sysvar,
                        equality_context: proofs.equality,
                        ciphertext_validity_context: proofs.ciphertext_validity,
                        range_context: proofs.range,
                        permissioned_burn_authority: key(proofs.next),
                        owner: key(proofs.next + 1),
                        multisig_signers: signers(proofs.next + 2),
                    },
                    args: burn_args(&data),
                })
            },
        };

        Ok(PermissionedBurnIx {
            instruction: Some(ix_msg),
        })
    }
}
