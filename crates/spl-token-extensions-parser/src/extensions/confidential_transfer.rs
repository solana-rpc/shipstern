use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Result};
use shipstern_proc_macro::shipstern;
use shipstern_spl_token_parser::InitializeMintAccounts;
use spl_token_2022::extension::confidential_transfer::instruction::ConfidentialTransferInstruction as SplConfidentialTransferInstruction;

use crate::{decode_extension_ix_type, ExtensionInstructionParser, Pubkey};

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateMintAccounts {
    pub mint: Pubkey,
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfigureAccountAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub sysvar: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ApproveAccountAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct EmptyAccountAccounts {
    pub account: Pubkey,
    pub sysvar: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawAccounts {
    pub source_account: Pubkey,
    pub mint: Pubkey,
    pub destination: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialTransferAccounts {
    pub source_account: Pubkey,
    pub mint: Pubkey,
    pub destination: Pubkey,
    pub context_account: Pubkey, // Sysvar account or context state account
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ApplyPendingBalanceAccounts {
    pub account: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CreditsAccounts {
    pub account: Pubkey,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct TransferWithFeeAccounts {
    pub source_account: Pubkey,
    pub mint: Pubkey,
    pub destination: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfigureAccountWithRegistryAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub registry: Pubkey,
    pub payer: Option<Pubkey>,
    pub system_program: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialTransferIx {
    #[hint(
        oneof = "confidential_transfer_instruction::Instruction",
        tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15"
    )]
    pub instruction: Option<confidential_transfer_instruction::Instruction>,
}

pub mod confidential_transfer_instruction {
    use super::*;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct InitializeMint {
        pub accounts: InitializeMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct UpdateMint {
        pub accounts: UpdateMintAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ConfigureAccount {
        pub accounts: ConfigureAccountAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ApproveAccount {
        pub accounts: ApproveAccountAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct EmptyAccount {
        pub accounts: EmptyAccountAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Deposit {
        pub accounts: DepositAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Withdraw {
        pub accounts: WithdrawAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Transfer {
        pub accounts: ConfidentialTransferAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ApplyPendingBalance {
        pub accounts: ApplyPendingBalanceAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct EnableConfidentialCredits {
        pub accounts: CreditsAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct DisableConfidentialCredits {
        pub accounts: CreditsAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct EnableNonConfidentialCredits {
        pub accounts: CreditsAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct DisableNonConfidentialCredits {
        pub accounts: CreditsAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct TransferWithFee {
        pub accounts: TransferWithFeeAccounts,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ConfigureAccountWithRegistry {
        pub accounts: ConfigureAccountWithRegistryAccounts,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        InitializeMint(InitializeMint),
        UpdateMint(UpdateMint),
        ConfigureAccount(ConfigureAccount),
        ApproveAccount(ApproveAccount),
        EmptyAccount(EmptyAccount),
        Deposit(Deposit),
        Withdraw(Withdraw),
        Transfer(Transfer),
        ApplyPendingBalance(ApplyPendingBalance),
        EnableConfidentialCredits(EnableConfidentialCredits),
        DisableConfidentialCredits(DisableConfidentialCredits),
        EnableNonConfidentialCredits(EnableNonConfidentialCredits),
        DisableNonConfidentialCredits(DisableNonConfidentialCredits),
        TransferWithFee(TransferWithFee),
        ConfigureAccountWithRegistry(ConfigureAccountWithRegistry),
    }
}

impl ExtensionInstructionParser for ConfidentialTransferIx {
    #[allow(clippy::too_many_lines)]
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();
        let ix_type: SplConfidentialTransferInstruction = decode_extension_ix_type(&ix.data[1..])?;

        use crate::confidential_transfer_instruction as oneof;

        let ix_msg = match ix_type {
            SplConfidentialTransferInstruction::InitializeMint => {
                check_min_accounts_req(accounts_len, 1)?;

                oneof::Instruction::InitializeMint(oneof::InitializeMint {
                    accounts: InitializeMintAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                    },
                })
            },

            SplConfidentialTransferInstruction::UpdateMint => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::UpdateMint(oneof::UpdateMint {
                    accounts: UpdateMintAccounts {
                        mint: crate::Pubkey::new(ix.accounts[0].0),
                        authority: crate::Pubkey::new(ix.accounts[1].0),
                    },
                })
            },

            SplConfidentialTransferInstruction::ConfigureAccount => {
                check_min_accounts_req(accounts_len, 4)?;

                oneof::Instruction::ConfigureAccount(oneof::ConfigureAccount {
                    accounts: ConfigureAccountAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        sysvar: crate::Pubkey::new(ix.accounts[2].0),
                        owner: crate::Pubkey::new(ix.accounts[3].0),
                        multisig_signers: ix.accounts[4..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::ApproveAccount => {
                check_min_accounts_req(accounts_len, 3)?;

                oneof::Instruction::ApproveAccount(oneof::ApproveAccount {
                    accounts: ApproveAccountAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        authority: crate::Pubkey::new(ix.accounts[2].0),
                    },
                })
            },

            SplConfidentialTransferInstruction::EmptyAccount => {
                check_min_accounts_req(accounts_len, 3)?;

                oneof::Instruction::EmptyAccount(oneof::EmptyAccount {
                    accounts: EmptyAccountAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        sysvar: crate::Pubkey::new(ix.accounts[1].0),
                        owner: crate::Pubkey::new(ix.accounts[2].0),
                        multisig_signers: ix.accounts[3..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::Deposit => {
                check_min_accounts_req(accounts_len, 3)?;

                oneof::Instruction::Deposit(oneof::Deposit {
                    accounts: DepositAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        owner: crate::Pubkey::new(ix.accounts[2].0),
                        multisig_signers: ix.accounts[3..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::Withdraw => {
                check_min_accounts_req(accounts_len, 4)?;

                oneof::Instruction::Withdraw(oneof::Withdraw {
                    accounts: WithdrawAccounts {
                        source_account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        destination: crate::Pubkey::new(ix.accounts[2].0),
                        owner: crate::Pubkey::new(ix.accounts[3].0),
                        multisig_signers: ix.accounts[4..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::Transfer => {
                check_min_accounts_req(accounts_len, 5)?;

                oneof::Instruction::Transfer(oneof::Transfer {
                    accounts: ConfidentialTransferAccounts {
                        source_account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        destination: crate::Pubkey::new(ix.accounts[2].0),
                        context_account: crate::Pubkey::new(ix.accounts[3].0),
                        owner: crate::Pubkey::new(ix.accounts[4].0),
                        multisig_signers: ix.accounts[5..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::ApplyPendingBalance => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::ApplyPendingBalance(oneof::ApplyPendingBalance {
                    accounts: ApplyPendingBalanceAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        owner: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::EnableConfidentialCredits => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::EnableConfidentialCredits(oneof::EnableConfidentialCredits {
                    accounts: CreditsAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        owner: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::DisableConfidentialCredits => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::DisableConfidentialCredits(oneof::DisableConfidentialCredits {
                    accounts: CreditsAccounts {
                        account: crate::Pubkey::new(ix.accounts[0].0),
                        owner: crate::Pubkey::new(ix.accounts[1].0),
                        multisig_signers: ix.accounts[2..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::EnableNonConfidentialCredits => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::EnableNonConfidentialCredits(
                    oneof::EnableNonConfidentialCredits {
                        accounts: CreditsAccounts {
                            account: crate::Pubkey::new(ix.accounts[0].0),
                            owner: crate::Pubkey::new(ix.accounts[1].0),
                            multisig_signers: ix.accounts[2..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                    },
                )
            },

            SplConfidentialTransferInstruction::DisableNonConfidentialCredits => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::DisableNonConfidentialCredits(
                    oneof::DisableNonConfidentialCredits {
                        accounts: CreditsAccounts {
                            account: crate::Pubkey::new(ix.accounts[0].0),
                            owner: crate::Pubkey::new(ix.accounts[1].0),
                            multisig_signers: ix.accounts[2..]
                                .iter()
                                .map(|a| crate::Pubkey::new(a.0))
                                .collect(),
                        },
                    },
                )
            },

            SplConfidentialTransferInstruction::TransferWithFee => {
                check_min_accounts_req(accounts_len, 5)?;

                oneof::Instruction::TransferWithFee(oneof::TransferWithFee {
                    accounts: TransferWithFeeAccounts {
                        source_account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        destination: crate::Pubkey::new(ix.accounts[2].0),
                    },
                })
            },

            SplConfidentialTransferInstruction::ConfigureAccountWithRegistry => {
                check_min_accounts_req(accounts_len, 3)?;

                oneof::Instruction::ConfigureAccountWithRegistry(
                    oneof::ConfigureAccountWithRegistry {
                        accounts: ConfigureAccountWithRegistryAccounts {
                            account: crate::Pubkey::new(ix.accounts[0].0),
                            mint: crate::Pubkey::new(ix.accounts[1].0),
                            registry: crate::Pubkey::new(ix.accounts[2].0),
                            payer: ix.accounts.get(3).map(|a| crate::Pubkey::new(a.0)),
                            system_program: ix.accounts.get(4).map(|a| crate::Pubkey::new(a.0)),
                        },
                    },
                )
            },
        };

        Ok(crate::ConfidentialTransferIx {
            instruction: Some(ix_msg),
        })
    }
}

#[cfg(test)]
mod tests {
    use shipstern_core::instruction::Path;
    use spl_token_2022::{
        extension::confidential_transfer::instruction::ConfidentialTransferInstruction,
        instruction::TokenInstruction,
    };

    use super::{confidential_transfer_instruction as oneof, *};

    fn pk(n: u8) -> Pubkey { Pubkey::new([n; 32]) }

    fn parse_configure_with_registry(
        accounts: &[u8],
    ) -> Result<ConfigureAccountWithRegistryAccounts> {
        let mut data = TokenInstruction::ConfidentialTransferExtension.pack();
        data.push(ConfidentialTransferInstruction::ConfigureAccountWithRegistry.into());

        let ix = InstructionUpdate {
            program: Pubkey::new(spl_token_2022::ID.to_bytes()),
            accounts: accounts.iter().map(|n| pk(*n)).collect(),
            data,
            shared: Default::default(),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        };

        let Some(oneof::Instruction::ConfigureAccountWithRegistry(c)) =
            ConfidentialTransferIx::try_parse(&ix)?.instruction
        else {
            panic!("expected ConfigureAccountWithRegistry");
        };

        Ok(c.accounts)
    }

    #[test]
    fn configure_with_registry_parses_without_payer_and_system_program() {
        let a = parse_configure_with_registry(&[1, 2, 3]).unwrap();

        assert_eq!(a.account, pk(1));
        assert_eq!(a.mint, pk(2));
        assert_eq!(a.registry, pk(3));
        assert_eq!(a.payer, None);
        assert_eq!(a.system_program, None);
    }

    #[test]
    fn configure_with_registry_reads_the_payer_and_system_program_for_reallocation() {
        let a = parse_configure_with_registry(&[1, 2, 3, 4, 5]).unwrap();

        assert_eq!(a.registry, pk(3));
        assert_eq!(a.payer, Some(pk(4)));
        assert_eq!(a.system_program, Some(pk(5)));
    }

    #[test]
    fn configure_with_registry_without_the_registry_is_rejected() {
        assert!(parse_configure_with_registry(&[1, 2]).is_err());
    }
}
