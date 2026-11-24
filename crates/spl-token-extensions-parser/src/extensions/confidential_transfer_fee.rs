use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Result};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::confidential_transfer_fee::instruction::ConfidentialTransferFeeInstruction as SplConfidentialTransferFeeInstruction;

use super::extension::{decode_extension_ix_type, ExtensionInstructionParser};
use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeConfidentialTransferFeeConfigAccounts {
    pub mint: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialWithdrawWithheldTokensFromMintAccounts {
    pub mint: Pubkey,
    pub fee_recipient: Pubkey,
    pub sysvar: Pubkey,
    pub withdraw_withheld_authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialWithdrawWithheldTokensFromAccounts {
    pub mint: Pubkey,
    pub fee_recipient: Pubkey,
    pub sysvar: Pubkey,
    pub withdraw_withheld_authority: Pubkey,
    pub source_accounts: Vec<Pubkey>,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialHarvestWithheldTokensToMintAccounts {
    pub mint: Pubkey,
    pub source_accounts: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct EnableHarvestToMintAccounts {
    pub mint: Pubkey,
    pub confidential_transfer_fee_authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DisableHarvestToMintAccounts {
    pub account: Pubkey,
    pub confidential_transfer_fee_authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialTransferFeeIx {
    #[hint(
        oneof = "confidential_transfer_fee_instruction::Instruction",
        tags = "1, 2, 3, 4, 5, 6"
    )]
    pub instruction: Option<confidential_transfer_fee_instruction::Instruction>,
}

pub mod confidential_transfer_fee_instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct InitializeConfidentialTransferFeeConfig {
        pub accounts: super::InitializeConfidentialTransferFeeConfigAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct WithdrawWithheldTokensFromMint {
        pub accounts: super::ConfidentialWithdrawWithheldTokensFromMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct WithdrawWithheldTokensFromAccounts {
        pub accounts: super::ConfidentialWithdrawWithheldTokensFromAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct HarvestWithheldTokensToMint {
        pub accounts: super::ConfidentialHarvestWithheldTokensToMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct EnableHarvestToMint {
        pub accounts: super::EnableHarvestToMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct DisableHarvestToMint {
        pub accounts: super::DisableHarvestToMintAccounts,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        InitializeConfidentialTransferFeeConfig(InitializeConfidentialTransferFeeConfig),

        WithdrawWithheldTokensFromMint(WithdrawWithheldTokensFromMint),

        WithdrawWithheldTokensFromAccounts(WithdrawWithheldTokensFromAccounts),

        HarvestWithheldTokensToMint(HarvestWithheldTokensToMint),

        EnableHarvestToMint(EnableHarvestToMint),

        DisableHarvestToMint(DisableHarvestToMint),
    }
}

impl ExtensionInstructionParser for ConfidentialTransferFeeIx {
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();
        let ix_type = decode_extension_ix_type(&ix.data[1..])?;

        use confidential_transfer_fee_instruction as oneof;

        let ix_msg = match ix_type {
            SplConfidentialTransferFeeInstruction::InitializeConfidentialTransferFeeConfig => {
                check_min_accounts_req(accounts_len, 1)?;

                oneof::Instruction::InitializeConfidentialTransferFeeConfig(
                    oneof::InitializeConfidentialTransferFeeConfig {
                        accounts: InitializeConfidentialTransferFeeConfigAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                        },
                    },
                )
            },

            SplConfidentialTransferFeeInstruction::WithdrawWithheldTokensFromMint => {
                check_min_accounts_req(accounts_len, 4)?;

                oneof::Instruction::WithdrawWithheldTokensFromMint(
                    oneof::WithdrawWithheldTokensFromMint {
                        accounts: ConfidentialWithdrawWithheldTokensFromMintAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            fee_recipient: crate::Pubkey::new(ix.accounts[1].0),
                            sysvar: crate::Pubkey::new(ix.accounts[2].0),
                            withdraw_withheld_authority: crate::Pubkey::new(ix.accounts[3].0),
                            multisig_signers: ix.accounts[4..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                    },
                )
            },

            SplConfidentialTransferFeeInstruction::WithdrawWithheldTokensFromAccounts => {
                check_min_accounts_req(accounts_len, 5)?;

                oneof::Instruction::WithdrawWithheldTokensFromAccounts(
                    oneof::WithdrawWithheldTokensFromAccounts {
                        accounts: ConfidentialWithdrawWithheldTokensFromAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            fee_recipient: crate::Pubkey::new(ix.accounts[1].0),
                            sysvar: crate::Pubkey::new(ix.accounts[2].0),
                            withdraw_withheld_authority: crate::Pubkey::new(ix.accounts[3].0),
                            source_accounts: ix.accounts[4..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                            multisig_signers: Vec::new(),
                        },
                    },
                )
            },

            SplConfidentialTransferFeeInstruction::HarvestWithheldTokensToMint => {
                // Only the mint is required; the program accepts an empty source list.
                check_min_accounts_req(accounts_len, 1)?;

                oneof::Instruction::HarvestWithheldTokensToMint(
                    oneof::HarvestWithheldTokensToMint {
                        accounts: ConfidentialHarvestWithheldTokensToMintAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            source_accounts: ix.accounts[1..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                    },
                )
            },

            SplConfidentialTransferFeeInstruction::EnableHarvestToMint => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::EnableHarvestToMint(oneof::EnableHarvestToMint {
                    accounts: EnableHarvestToMintAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                        confidential_transfer_fee_authority: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferFeeInstruction::DisableHarvestToMint => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::DisableHarvestToMint(oneof::DisableHarvestToMint {
                    accounts: DisableHarvestToMintAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        confidential_transfer_fee_authority: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },
        };

        Ok(ConfidentialTransferFeeIx {
            instruction: Some(ix_msg),
        })
    }
}
