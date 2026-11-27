use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Result};
use shipstern_proc_macro::shipstern;
use shipstern_spl_token_parser::InitializeMintAccounts;
use spl_token_2022::extension::confidential_transfer::instruction::{
    ConfidentialTransferInstruction as SplConfidentialTransferInstruction, TransferInstructionData,
    TransferWithFeeInstructionData,
};

use crate::{
    decode_extension_ix_data, decode_extension_ix_type, ExtensionInstructionParser, Pubkey,
};

/// The accounts that sit between the fixed accounts and the owner of a proof-carrying instruction.
struct ProofAccounts<const N: usize> {
    sysvar: Option<Pubkey>,
    /// One slot per proof, in the order of the offsets. `Some` when the offset is `0`.
    contexts: [Option<Pubkey>; N],
    owner_index: usize,
}

/// Token-2022 reads the instructions sysvar if any proof offset is not `0`, then one context
/// state account per proof whose offset is `0`, then the owner.
fn read_proof_accounts<const N: usize>(
    ix: &InstructionUpdate,
    first: usize,
    offsets: [i8; N],
) -> Result<ProofAccounts<N>> {
    let has_sysvar = offsets.iter().any(|offset| *offset != 0);
    let context_count = offsets.iter().filter(|offset| **offset == 0).count();
    let owner_index = first + usize::from(has_sysvar) + context_count;

    check_min_accounts_req(ix.accounts.len(), owner_index + 1)?;

    let sysvar = has_sysvar.then(|| Pubkey::new(ix.accounts[first].0));

    let mut next = first + usize::from(has_sysvar);
    let contexts = offsets.map(|offset| {
        if offset != 0 {
            return None;
        }

        let context = Pubkey::new(ix.accounts[next].0);
        next += 1;

        Some(context)
    });

    Ok(ProofAccounts {
        sysvar,
        contexts,
        owner_index,
    })
}

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
    pub instructions_sysvar: Option<Pubkey>,
    pub equality_context: Option<Pubkey>,
    pub ciphertext_validity_context: Option<Pubkey>,
    pub range_context: Option<Pubkey>,
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
    pub instructions_sysvar: Option<Pubkey>,
    pub equality_context: Option<Pubkey>,
    pub transfer_amount_ciphertext_validity_context: Option<Pubkey>,
    pub fee_sigma_context: Option<Pubkey>,
    pub fee_ciphertext_validity_context: Option<Pubkey>,
    pub range_context: Option<Pubkey>,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfigureAccountWithRegistryAccounts {
    pub account: Pubkey,
    pub mint: Pubkey,
    pub registry: Pubkey,
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
                check_min_accounts_req(accounts_len, 3)?;

                let data = decode_extension_ix_data::<TransferInstructionData>(&ix.data)?;
                let proofs = read_proof_accounts(ix, 3, [
                    data.equality_proof_instruction_offset,
                    data.ciphertext_validity_proof_instruction_offset,
                    data.range_proof_instruction_offset,
                ])?;
                let [equality_context, ciphertext_validity_context, range_context] =
                    proofs.contexts;

                oneof::Instruction::Transfer(oneof::Transfer {
                    accounts: ConfidentialTransferAccounts {
                        source_account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        destination: crate::Pubkey::new(ix.accounts[2].0),
                        instructions_sysvar: proofs.sysvar,
                        equality_context,
                        ciphertext_validity_context,
                        range_context,
                        owner: crate::Pubkey::new(ix.accounts[proofs.owner_index].0),
                        multisig_signers: ix.accounts[(proofs.owner_index + 1)..]
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
                check_min_accounts_req(accounts_len, 3)?;

                let data = decode_extension_ix_data::<TransferWithFeeInstructionData>(&ix.data)?;
                let proofs = read_proof_accounts(ix, 3, [
                    data.equality_proof_instruction_offset,
                    data.transfer_amount_ciphertext_validity_proof_instruction_offset,
                    data.fee_sigma_proof_instruction_offset,
                    data.fee_ciphertext_validity_proof_instruction_offset,
                    data.range_proof_instruction_offset,
                ])?;
                let [equality_context, transfer_amount_ciphertext_validity_context, fee_sigma_context, fee_ciphertext_validity_context, range_context] =
                    proofs.contexts;

                oneof::Instruction::TransferWithFee(oneof::TransferWithFee {
                    accounts: TransferWithFeeAccounts {
                        source_account: crate::Pubkey::new(ix.accounts[0].0),
                        mint: crate::Pubkey::new(ix.accounts[1].0),
                        destination: crate::Pubkey::new(ix.accounts[2].0),
                        instructions_sysvar: proofs.sysvar,
                        equality_context,
                        transfer_amount_ciphertext_validity_context,
                        fee_sigma_context,
                        fee_ciphertext_validity_context,
                        range_context,
                        owner: crate::Pubkey::new(ix.accounts[proofs.owner_index].0),
                        multisig_signers: ix.accounts[(proofs.owner_index + 1)..]
                            .iter()
                            .map(|a| crate::Pubkey::new(a.0))
                            .collect(),
                    },
                })
            },

            SplConfidentialTransferInstruction::ConfigureAccountWithRegistry => {
                check_min_accounts_req(accounts_len, 4)?;

                oneof::Instruction::ConfigureAccountWithRegistry(
                    oneof::ConfigureAccountWithRegistry {
                        accounts: ConfigureAccountWithRegistryAccounts {
                            account: crate::Pubkey::new(ix.accounts[0].0),
                            mint: crate::Pubkey::new(ix.accounts[1].0),
                            registry: crate::Pubkey::new(ix.accounts[2].0),
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
    use bytemuck::Zeroable;
    use shipstern_core::instruction::Path;
    use spl_token_2022::{
        extension::confidential_transfer::instruction::ConfidentialTransferInstruction,
        instruction::TokenInstruction,
    };

    use super::{confidential_transfer_instruction as oneof, *};

    fn pk(n: u8) -> Pubkey { Pubkey::new([n; 32]) }

    fn update(data: Vec<u8>, accounts: &[u8]) -> InstructionUpdate {
        InstructionUpdate {
            program: Pubkey::new(spl_token_2022::ID.to_bytes()),
            accounts: accounts.iter().map(|n| pk(*n)).collect(),
            data,
            shared: Default::default(),
            inner: vec![],
            path: Path::new_single(0),
            log_range: 0..0,
        }
    }

    fn encode<D: bytemuck::Pod>(ix_type: ConfidentialTransferInstruction, data: &D) -> Vec<u8> {
        let mut bytes = TokenInstruction::ConfidentialTransferExtension.pack();
        bytes.push(ix_type.into());
        bytes.extend_from_slice(bytemuck::bytes_of(data));

        bytes
    }

    fn parse_transfer(offsets: [i8; 3], accounts: &[u8]) -> Result<ConfidentialTransferAccounts> {
        let mut data = TransferInstructionData::zeroed();
        data.equality_proof_instruction_offset = offsets[0];
        data.ciphertext_validity_proof_instruction_offset = offsets[1];
        data.range_proof_instruction_offset = offsets[2];

        let ix = update(
            encode(ConfidentialTransferInstruction::Transfer, &data),
            accounts,
        );

        let Some(oneof::Instruction::Transfer(t)) =
            ConfidentialTransferIx::try_parse(&ix)?.instruction
        else {
            panic!("expected Transfer");
        };

        Ok(t.accounts)
    }

    fn parse_transfer_with_fee(
        offsets: [i8; 5],
        accounts: &[u8],
    ) -> Result<TransferWithFeeAccounts> {
        let mut data = TransferWithFeeInstructionData::zeroed();
        data.equality_proof_instruction_offset = offsets[0];
        data.transfer_amount_ciphertext_validity_proof_instruction_offset = offsets[1];
        data.fee_sigma_proof_instruction_offset = offsets[2];
        data.fee_ciphertext_validity_proof_instruction_offset = offsets[3];
        data.range_proof_instruction_offset = offsets[4];

        let ix = update(
            encode(ConfidentialTransferInstruction::TransferWithFee, &data),
            accounts,
        );

        let Some(oneof::Instruction::TransferWithFee(t)) =
            ConfidentialTransferIx::try_parse(&ix)?.instruction
        else {
            panic!("expected TransferWithFee");
        };

        Ok(t.accounts)
    }

    #[test]
    fn transfer_with_all_proofs_in_instructions_reads_the_sysvar() {
        // [source, mint, destination, instructions sysvar, owner]
        let a = parse_transfer([1, 2, 3], &[1, 2, 3, 4, 5]).unwrap();

        assert_eq!(a.destination, pk(3));
        assert_eq!(a.instructions_sysvar, Some(pk(4)));
        assert_eq!(a.equality_context, None);
        assert_eq!(a.ciphertext_validity_context, None);
        assert_eq!(a.range_context, None);
        assert_eq!(a.owner, pk(5));
        assert!(a.multisig_signers.is_empty());
    }

    #[test]
    fn transfer_with_all_proofs_in_context_accounts_has_no_sysvar() {
        // [source, mint, destination, equality, ciphertext validity, range, owner]
        let a = parse_transfer([0, 0, 0], &[1, 2, 3, 4, 5, 6, 7]).unwrap();

        assert_eq!(a.instructions_sysvar, None);
        assert_eq!(a.equality_context, Some(pk(4)));
        assert_eq!(a.ciphertext_validity_context, Some(pk(5)));
        assert_eq!(a.range_context, Some(pk(6)));
        assert_eq!(a.owner, pk(7));
    }

    #[test]
    fn transfer_with_mixed_proofs_keeps_each_context_in_its_slot() {
        // Only the ciphertext validity proof is pre-verified.
        let a = parse_transfer([1, 0, 3], &[1, 2, 3, 4, 5, 6]).unwrap();

        assert_eq!(a.instructions_sysvar, Some(pk(4)));
        assert_eq!(a.equality_context, None);
        assert_eq!(a.ciphertext_validity_context, Some(pk(5)));
        assert_eq!(a.range_context, None);
        assert_eq!(a.owner, pk(6));
    }

    #[test]
    fn transfer_with_multisig_owner_lists_the_signers() {
        let a = parse_transfer([0, 0, 0], &[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();

        assert_eq!(a.owner, pk(7));
        assert_eq!(a.multisig_signers, vec![pk(8), pk(9)]);
    }

    #[test]
    fn transfer_without_the_owner_account_is_rejected() {
        assert!(parse_transfer([1, 2, 3], &[1, 2, 3, 4]).is_err());
        assert!(parse_transfer([0, 0, 0], &[1, 2, 3, 4, 5, 6]).is_err());
    }

    #[test]
    fn transfer_with_fee_with_all_proofs_in_instructions_reads_the_owner() {
        let a = parse_transfer_with_fee([1, 2, 3, 4, 5], &[1, 2, 3, 4, 5]).unwrap();

        assert_eq!(a.destination, pk(3));
        assert_eq!(a.instructions_sysvar, Some(pk(4)));
        assert_eq!(a.equality_context, None);
        assert_eq!(a.transfer_amount_ciphertext_validity_context, None);
        assert_eq!(a.fee_sigma_context, None);
        assert_eq!(a.fee_ciphertext_validity_context, None);
        assert_eq!(a.range_context, None);
        assert_eq!(a.owner, pk(5));
    }

    #[test]
    fn transfer_with_fee_with_all_proofs_in_context_accounts_reads_five_contexts() {
        let a = parse_transfer_with_fee([0; 5], &[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();

        assert_eq!(a.instructions_sysvar, None);
        assert_eq!(a.equality_context, Some(pk(4)));
        assert_eq!(a.transfer_amount_ciphertext_validity_context, Some(pk(5)));
        assert_eq!(a.fee_sigma_context, Some(pk(6)));
        assert_eq!(a.fee_ciphertext_validity_context, Some(pk(7)));
        assert_eq!(a.range_context, Some(pk(8)));
        assert_eq!(a.owner, pk(9));
    }

    #[test]
    fn transfer_with_fee_with_mixed_proofs_and_multisig_owner() {
        // Fee sigma and fee ciphertext validity proofs are pre-verified.
        let a = parse_transfer_with_fee([1, 2, 0, 0, 5], &[1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();

        assert_eq!(a.instructions_sysvar, Some(pk(4)));
        assert_eq!(a.equality_context, None);
        assert_eq!(a.transfer_amount_ciphertext_validity_context, None);
        assert_eq!(a.fee_sigma_context, Some(pk(5)));
        assert_eq!(a.fee_ciphertext_validity_context, Some(pk(6)));
        assert_eq!(a.range_context, None);
        assert_eq!(a.owner, pk(7));
        assert_eq!(a.multisig_signers, vec![pk(8), pk(9)]);
    }

    #[test]
    fn transfer_with_fee_without_the_owner_account_is_rejected() {
        assert!(parse_transfer_with_fee([1, 2, 3, 4, 5], &[1, 2, 3, 4]).is_err());
        assert!(parse_transfer_with_fee([0; 5], &[1, 2, 3, 4, 5, 6, 7, 8]).is_err());
    }
}
