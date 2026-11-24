use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Result, ResultExt};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::transfer_fee::instruction::TransferFeeInstruction as SplTransferFeeInstruction;

use super::extension::ExtensionInstructionParser;
use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct TransferCheckedWithFeeAccounts {
    pub source: Pubkey,
    pub mint: Pubkey,
    pub destination: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct TransferCheckedWithFeeArgs {
    pub amount: u64,
    pub fee_amount: u64,
    // u8 -> uint32 in proto
    pub decimals: u32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeTransferFeeConfigAccounts {
    pub mint: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeTransferFeeConfigArgs {
    pub transfer_fee_config_authority: Option<Pubkey>,
    pub withdraw_withheld_authority: Option<Pubkey>,
    // u16 -> uint32 in proto
    pub transfer_fee_basis_points: u32,
    pub maximum_fee: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawWithheldTokensFromMintAccounts {
    pub mint: Pubkey,
    pub fee_recipient: Pubkey,
    pub withdraw_withheld_authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawWithheldTokensFromAccountsAccounts {
    pub mint: Pubkey,
    pub fee_recipient: Pubkey,
    pub withdraw_withheld_authority: Pubkey,
    pub source_accounts: Vec<Pubkey>,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawWithheldTokensFromAccountsArgs {
    // u8 -> uint32 in proto
    pub num_token_accounts: u32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetTransferFeeAccounts {
    pub mint: Pubkey,
    pub mint_fee_acc_owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetTransferFeeArgs {
    // u16 -> uint32 in proto
    pub transfer_fee_basis_points: u32,
    pub maximum_fee: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct HarvestWithheldTokensToMintAccounts {
    pub mint: Pubkey,
    pub mint_fee_acc_owner: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct TransferFeeIx {
    #[hint(
        oneof = "transfer_fee_instruction::Instruction",
        tags = "1, 2, 3, 4, 5, 6"
    )]
    pub instruction: Option<transfer_fee_instruction::Instruction>,
}

pub mod transfer_fee_instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct TransferCheckedWithFee {
        pub accounts: super::TransferCheckedWithFeeAccounts,
        pub args: super::TransferCheckedWithFeeArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct InitializeTransferFeeConfig {
        pub accounts: super::InitializeTransferFeeConfigAccounts,
        pub args: super::InitializeTransferFeeConfigArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct WithdrawWithheldTokensFromMint {
        pub accounts: super::WithdrawWithheldTokensFromMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct WithdrawWithheldTokensFromAccounts {
        pub accounts: super::WithdrawWithheldTokensFromAccountsAccounts,
        pub args: super::WithdrawWithheldTokensFromAccountsArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct HarvestWithheldTokensToMint {
        pub accounts: super::HarvestWithheldTokensToMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct SetTransferFee {
        pub accounts: super::SetTransferFeeAccounts,
        pub args: super::SetTransferFeeArgs,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        TransferCheckedWithFee(TransferCheckedWithFee),
        InitializeTransferFeeConfig(InitializeTransferFeeConfig),
        WithdrawWithheldTokensFromMint(WithdrawWithheldTokensFromMint),
        WithdrawWithheldTokensFromAccounts(WithdrawWithheldTokensFromAccounts),
        HarvestWithheldTokensToMint(HarvestWithheldTokensToMint),
        SetTransferFee(SetTransferFee),
    }
}

impl ExtensionInstructionParser for TransferFeeIx {
    #[allow(clippy::too_many_lines)]
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();

        let ix_type = SplTransferFeeInstruction::unpack(&ix.data[1..])
            .parse_err("Error unpacking transfer fee instruction data")?;

        use transfer_fee_instruction as oneof;

        let ix_msg = match ix_type {
            SplTransferFeeInstruction::TransferCheckedWithFee {
                amount,
                decimals,
                fee,
            } => {
                check_min_accounts_req(accounts_len, 4)?;

                oneof::Instruction::TransferCheckedWithFee(oneof::TransferCheckedWithFee {
                    accounts: TransferCheckedWithFeeAccounts {
                        source: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        destination: crate::Pubkey::new(ix.accounts[2].0),
                        owner: crate::Pubkey::new(ix.accounts[3].0),
                        multisig_signers: ix.accounts[4..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                    args: TransferCheckedWithFeeArgs {
                        amount,
                        fee_amount: fee,
                        decimals: decimals as u32,
                    },
                })
            },

            SplTransferFeeInstruction::InitializeTransferFeeConfig {
                transfer_fee_config_authority,
                withdraw_withheld_authority,
                transfer_fee_basis_points,
                maximum_fee,
            } => {
                check_min_accounts_req(accounts_len, 1)?;

                oneof::Instruction::InitializeTransferFeeConfig(
                    oneof::InitializeTransferFeeConfig {
                        accounts: InitializeTransferFeeConfigAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                        },
                        args: InitializeTransferFeeConfigArgs {
                            transfer_fee_config_authority: transfer_fee_config_authority
                                .map(|p| crate::Pubkey::new(p.to_bytes()))
                                .into(),
                            withdraw_withheld_authority: withdraw_withheld_authority
                                .map(|p| crate::Pubkey::new(p.to_bytes()))
                                .into(),
                            transfer_fee_basis_points: transfer_fee_basis_points as u32,
                            maximum_fee,
                        },
                    },
                )
            },

            SplTransferFeeInstruction::WithdrawWithheldTokensFromMint => {
                check_min_accounts_req(accounts_len, 3)?;

                oneof::Instruction::WithdrawWithheldTokensFromMint(
                    oneof::WithdrawWithheldTokensFromMint {
                        accounts: WithdrawWithheldTokensFromMintAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            fee_recipient: crate::Pubkey::new(ix.accounts[1].0),
                            withdraw_withheld_authority: crate::Pubkey::new(ix.accounts[2].0),
                            multisig_signers: ix.accounts[3..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                    },
                )
            },

            SplTransferFeeInstruction::WithdrawWithheldTokensFromAccounts {
                num_token_accounts,
            } => {
                check_min_accounts_req(accounts_len, 3)?;

                // As the program does: the last `num_token_accounts` accounts are sources, and
                // any before them are signers. A list shorter than `num_token_accounts` is valid
                // on chain and holds only sources.
                let n = (num_token_accounts as usize).min(accounts_len - 3);

                oneof::Instruction::WithdrawWithheldTokensFromAccounts(
                    oneof::WithdrawWithheldTokensFromAccounts {
                        accounts: WithdrawWithheldTokensFromAccountsAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            fee_recipient: crate::Pubkey::new(ix.accounts[1].0),
                            withdraw_withheld_authority: crate::Pubkey::new(ix.accounts[2].0),
                            multisig_signers: ix.accounts[3..(accounts_len - n)]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                            source_accounts: ix.accounts[(accounts_len - n)..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                        args: WithdrawWithheldTokensFromAccountsArgs {
                            num_token_accounts: num_token_accounts as u32,
                        },
                    },
                )
            },

            SplTransferFeeInstruction::HarvestWithheldTokensToMint => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::HarvestWithheldTokensToMint(
                    oneof::HarvestWithheldTokensToMint {
                        accounts: HarvestWithheldTokensToMintAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            mint_fee_acc_owner: crate::Pubkey::new(ix.accounts[1].0),
                        },
                    },
                )
            },

            SplTransferFeeInstruction::SetTransferFee {
                transfer_fee_basis_points,
                maximum_fee,
            } => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::SetTransferFee(oneof::SetTransferFee {
                    accounts: SetTransferFeeAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                        mint_fee_acc_owner: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                    args: SetTransferFeeArgs {
                        transfer_fee_basis_points: transfer_fee_basis_points as u32,
                        maximum_fee,
                    },
                })
            },
        };

        Ok(TransferFeeIx {
            instruction: Some(ix_msg),
        })
    }
}
