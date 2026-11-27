use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Result};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::confidential_transfer_fee::instruction::{
    ConfidentialTransferFeeInstruction as SplConfidentialTransferFeeInstruction,
    WithdrawWithheldTokensFromAccountsData,
};

use super::extension::{
    decode_extension_ix_data, decode_extension_ix_type, ExtensionInstructionParser,
};
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
                let data =
                    decode_extension_ix_data::<WithdrawWithheldTokensFromAccountsData>(&ix.data)?;

                check_min_accounts_req(accounts_len, 4)?;

                // As the program does: the last `num_token_accounts` accounts are sources, and
                // any before them are signers. A list shorter than `num_token_accounts` is valid
                // on chain and holds only sources.
                let n = (data.num_token_accounts as usize).min(accounts_len - 4);

                oneof::Instruction::WithdrawWithheldTokensFromAccounts(
                    oneof::WithdrawWithheldTokensFromAccounts {
                        accounts: ConfidentialWithdrawWithheldTokensFromAccounts {
                            mint: crate::Pubkey::new(ix.accounts[0].0),
                            fee_recipient: crate::Pubkey::new(ix.accounts[1].0),
                            sysvar: crate::Pubkey::new(ix.accounts[2].0),
                            withdraw_withheld_authority: crate::Pubkey::new(ix.accounts[3].0),
                            source_accounts: ix.accounts[(accounts_len - n)..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                            multisig_signers: ix.accounts[4..(accounts_len - n)]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                    },
                )
            },

            SplConfidentialTransferFeeInstruction::HarvestWithheldTokensToMint => {
                check_min_accounts_req(accounts_len, 2)?;

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

#[cfg(test)]
mod tests {
    use bytemuck::Zeroable;
    use shipstern_core::instruction::Path;
    use spl_token_2022::{
        extension::confidential_transfer_fee::instruction::ConfidentialTransferFeeInstruction,
        instruction::TokenInstruction,
    };

    use super::{confidential_transfer_fee_instruction as oneof, *};

    fn pk(n: u8) -> Pubkey { Pubkey::new([n; 32]) }

    fn parse_withdraw_from_accounts(
        num_token_accounts: u8,
        accounts: &[u8],
    ) -> Result<ConfidentialWithdrawWithheldTokensFromAccounts> {
        let mut args = WithdrawWithheldTokensFromAccountsData::zeroed();
        args.num_token_accounts = num_token_accounts;

        let mut data = TokenInstruction::ConfidentialTransferFeeExtension.pack();
        data.push(ConfidentialTransferFeeInstruction::WithdrawWithheldTokensFromAccounts.into());
        data.extend_from_slice(bytemuck::bytes_of(&args));

        let ix = InstructionUpdate {
            program: Pubkey::new(spl_token_2022::ID.to_bytes()),
            accounts: accounts.iter().map(|n| pk(*n)).collect(),
            data,
            shared: Default::default(),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        };

        let Some(oneof::Instruction::WithdrawWithheldTokensFromAccounts(w)) =
            ConfidentialTransferFeeIx::try_parse(&ix)?.instruction
        else {
            panic!("expected WithdrawWithheldTokensFromAccounts");
        };

        Ok(w.accounts)
    }

    #[test]
    fn withdraw_from_accounts_with_a_single_authority_lists_the_sources() {
        // [mint, fee recipient, sysvar or context, authority, sources...]
        let a = parse_withdraw_from_accounts(2, &[1, 2, 3, 4, 5, 6]).unwrap();

        assert_eq!(a.withdraw_withheld_authority, pk(4));
        assert!(a.multisig_signers.is_empty());
        assert_eq!(a.source_accounts, vec![pk(5), pk(6)]);
    }

    #[test]
    fn withdraw_from_accounts_reads_multisig_signers_before_the_sources() {
        // [mint, fee recipient, sysvar or context, multisig authority, signers..., sources...]
        let a = parse_withdraw_from_accounts(3, &[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();

        assert_eq!(a.withdraw_withheld_authority, pk(4));
        assert_eq!(a.multisig_signers, vec![pk(5), pk(6)]);
        assert_eq!(a.source_accounts, vec![pk(7), pk(8), pk(9)]);
    }

    #[test]
    fn withdraw_from_accounts_with_no_sources_keeps_the_signers() {
        let a = parse_withdraw_from_accounts(0, &[1, 2, 3, 4, 5, 6]).unwrap();

        assert_eq!(a.multisig_signers, vec![pk(5), pk(6)]);
        assert!(a.source_accounts.is_empty());
    }

    #[test]
    fn withdraw_from_accounts_with_fewer_sources_than_declared_lists_them() {
        // The program takes every account after the authority as a source in this case.
        let a = parse_withdraw_from_accounts(2, &[1, 2, 3, 4, 5]).unwrap();

        assert!(a.multisig_signers.is_empty());
        assert_eq!(a.source_accounts, vec![pk(5)]);
    }

    #[test]
    fn withdraw_from_accounts_without_an_authority_is_rejected() {
        assert!(parse_withdraw_from_accounts(0, &[1, 2, 3]).is_err());
    }
}
