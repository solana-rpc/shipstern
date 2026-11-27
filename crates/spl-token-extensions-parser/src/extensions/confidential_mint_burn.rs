use bytemuck::{bytes_of, try_pod_read_unaligned, Pod};
use shipstern_core::instruction::InstructionUpdate;
use shipstern_parser::{check_min_accounts_req, Error, Result};
use shipstern_proc_macro::shipstern;
use spl_token_2022::extension::confidential_mint_burn::instruction::{
    BurnInstructionData, ConfidentialMintBurnInstruction as SplConfidentialMintBurnInstruction,
    InitializeMintData, MintInstructionData, RotateSupplyElGamalPubkeyData,
    UpdateDecryptableSupplyData,
};

use super::extension::{decode_extension_ix_type, ExtensionInstructionParser};
use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialMintBurnInitializeMintAccounts {
    pub mint: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialMintBurnInitializeMintArgs {
    pub supply_elgamal_pubkey: Vec<u8>,
    pub decryptable_supply: Vec<u8>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct RotateSupplyElGamalPubkeyAccounts {
    pub mint: Pubkey,
    /// Set when the proof is in the same transaction.
    pub instructions_sysvar: Option<Pubkey>,
    /// Set when the proof is pre-verified in a context state account.
    pub equality_context: Option<Pubkey>,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct RotateSupplyElGamalPubkeyArgs {
    pub new_supply_elgamal_pubkey: Vec<u8>,
    pub proof_instruction_offset: i32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateDecryptableSupplyAccounts {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateDecryptableSupplyArgs {
    pub new_decryptable_supply: Vec<u8>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialMintAccounts {
    pub token_account: Pubkey,
    pub mint: Pubkey,
    pub instructions_sysvar: Option<Pubkey>,
    pub equality_context: Option<Pubkey>,
    pub ciphertext_validity_context: Option<Pubkey>,
    pub range_context: Option<Pubkey>,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialMintArgs {
    pub new_decryptable_supply: Vec<u8>,
    pub mint_amount_auditor_ciphertext_lo: Vec<u8>,
    pub mint_amount_auditor_ciphertext_hi: Vec<u8>,
    pub equality_proof_instruction_offset: i32,
    pub ciphertext_validity_proof_instruction_offset: i32,
    pub range_proof_instruction_offset: i32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialBurnAccounts {
    pub token_account: Pubkey,
    pub mint: Pubkey,
    pub instructions_sysvar: Option<Pubkey>,
    pub equality_context: Option<Pubkey>,
    pub ciphertext_validity_context: Option<Pubkey>,
    pub range_context: Option<Pubkey>,
    pub owner: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialBurnArgs {
    pub new_decryptable_available_balance: Vec<u8>,
    pub burn_amount_auditor_ciphertext_lo: Vec<u8>,
    pub burn_amount_auditor_ciphertext_hi: Vec<u8>,
    pub equality_proof_instruction_offset: i32,
    pub ciphertext_validity_proof_instruction_offset: i32,
    pub range_proof_instruction_offset: i32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ApplyPendingBurnAccounts {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub multisig_signers: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ConfidentialMintBurnIx {
    #[hint(
        oneof = "confidential_mint_burn_instruction::Instruction",
        tags = "1, 2, 3, 4, 5, 6"
    )]
    pub instruction: Option<confidential_mint_burn_instruction::Instruction>,
}

pub mod confidential_mint_burn_instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct InitializeMint {
        pub accounts: super::ConfidentialMintBurnInitializeMintAccounts,
        pub args: super::ConfidentialMintBurnInitializeMintArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct RotateSupplyElGamalPubkey {
        pub accounts: super::RotateSupplyElGamalPubkeyAccounts,
        pub args: super::RotateSupplyElGamalPubkeyArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct UpdateDecryptableSupply {
        pub accounts: super::UpdateDecryptableSupplyAccounts,
        pub args: super::UpdateDecryptableSupplyArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Mint {
        pub accounts: super::ConfidentialMintAccounts,
        pub args: super::ConfidentialMintArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Burn {
        pub accounts: super::ConfidentialBurnAccounts,
        pub args: super::ConfidentialBurnArgs,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ApplyPendingBurn {
        pub accounts: super::ApplyPendingBurnAccounts,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        InitializeMint(InitializeMint),
        RotateSupplyElGamalPubkey(RotateSupplyElGamalPubkey),
        UpdateDecryptableSupply(UpdateDecryptableSupply),
        Mint(Mint),
        Burn(Burn),
        ApplyPendingBurn(ApplyPendingBurn),
    }
}

/// The accounts that sit between the fixed accounts and the authority of a mint or burn.
pub(crate) struct ProofAccounts {
    pub sysvar: Option<Pubkey>,
    pub equality: Option<Pubkey>,
    pub ciphertext_validity: Option<Pubkey>,
    pub range: Option<Pubkey>,
    /// Index of the first account after the proof accounts.
    pub next: usize,
}

/// Token-2022 reads the instructions sysvar if any proof offset is not `0`, then one context
/// state account for each proof whose offset is `0`, in the order equality, ciphertext validity,
/// range.
pub(crate) fn read_mint_burn_proof_accounts(
    ix: &InstructionUpdate,
    first: usize,
    [equality, ciphertext_validity, range]: [i8; 3],
) -> Result<ProofAccounts> {
    let has_sysvar = [equality, ciphertext_validity, range]
        .iter()
        .any(|offset| *offset != 0);
    let context_count = [equality, ciphertext_validity, range]
        .iter()
        .filter(|offset| **offset == 0)
        .count();

    check_min_accounts_req(
        ix.accounts.len(),
        first + usize::from(has_sysvar) + context_count,
    )?;

    let mut next = first;
    let mut take = |present: bool| {
        if !present {
            return None;
        }

        let account = Pubkey::new(ix.accounts[next].0);
        next += 1;

        Some(account)
    };

    let sysvar = take(has_sysvar);
    let equality = take(equality == 0);
    let ciphertext_validity = take(ciphertext_validity == 0);
    let range = take(range == 0);

    Ok(ProofAccounts {
        sysvar,
        equality,
        ciphertext_validity,
        range,
        next,
    })
}

pub(crate) fn decode_data<T: Pod>(ix: &InstructionUpdate, what: &str) -> Result<T> {
    try_pod_read_unaligned(&ix.data[2..]).map_err(|_| Error::new(format!("Invalid {what} data")))
}

impl ExtensionInstructionParser for ConfidentialMintBurnIx {
    fn try_parse(ix: &InstructionUpdate) -> Result<Self> {
        let accounts_len = ix.accounts.len();
        let ix_type: SplConfidentialMintBurnInstruction = decode_extension_ix_type(&ix.data[1..])?;

        use confidential_mint_burn_instruction as oneof;

        let key = |i: usize| Pubkey::new(ix.accounts[i].0);
        let signers = |from: usize| -> Vec<Pubkey> {
            ix.accounts[from..]
                .iter()
                .map(|a| Pubkey::new(a.0))
                .collect()
        };

        let ix_msg = match ix_type {
            SplConfidentialMintBurnInstruction::InitializeMint => {
                check_min_accounts_req(accounts_len, 1)?;

                let data: InitializeMintData = decode_data(ix, "confidential mint initialize")?;

                oneof::Instruction::InitializeMint(oneof::InitializeMint {
                    accounts: ConfidentialMintBurnInitializeMintAccounts { mint: key(0) },
                    args: ConfidentialMintBurnInitializeMintArgs {
                        supply_elgamal_pubkey: bytes_of(&data.supply_elgamal_pubkey).to_vec(),
                        decryptable_supply: bytes_of(&data.decryptable_supply).to_vec(),
                    },
                })
            },

            SplConfidentialMintBurnInstruction::RotateSupplyElGamalPubkey => {
                // [mint, sysvar or context state account, authority, signers..]
                check_min_accounts_req(accounts_len, 3)?;

                let data: RotateSupplyElGamalPubkeyData =
                    decode_data(ix, "rotate supply ElGamal pubkey")?;
                let in_transaction = data.proof_instruction_offset != 0;

                oneof::Instruction::RotateSupplyElGamalPubkey(oneof::RotateSupplyElGamalPubkey {
                    accounts: RotateSupplyElGamalPubkeyAccounts {
                        mint: key(0),
                        instructions_sysvar: in_transaction.then(|| key(1)),
                        equality_context: (!in_transaction).then(|| key(1)),
                        authority: key(2),
                        multisig_signers: signers(3),
                    },
                    args: RotateSupplyElGamalPubkeyArgs {
                        new_supply_elgamal_pubkey: bytes_of(&data.new_supply_elgamal_pubkey)
                            .to_vec(),
                        proof_instruction_offset: data.proof_instruction_offset.into(),
                    },
                })
            },

            SplConfidentialMintBurnInstruction::UpdateDecryptableSupply => {
                check_min_accounts_req(accounts_len, 2)?;

                let data: UpdateDecryptableSupplyData =
                    decode_data(ix, "update decryptable supply")?;

                oneof::Instruction::UpdateDecryptableSupply(oneof::UpdateDecryptableSupply {
                    accounts: UpdateDecryptableSupplyAccounts {
                        mint: key(0),
                        authority: key(1),
                        multisig_signers: signers(2),
                    },
                    args: UpdateDecryptableSupplyArgs {
                        new_decryptable_supply: bytes_of(&data.new_decryptable_supply).to_vec(),
                    },
                })
            },

            SplConfidentialMintBurnInstruction::Mint => {
                check_min_accounts_req(accounts_len, 2)?;

                let data: MintInstructionData = decode_data(ix, "confidential mint")?;
                let proofs = read_mint_burn_proof_accounts(ix, 2, [
                    data.equality_proof_instruction_offset,
                    data.ciphertext_validity_proof_instruction_offset,
                    data.range_proof_instruction_offset,
                ])?;
                check_min_accounts_req(accounts_len, proofs.next + 1)?;

                oneof::Instruction::Mint(oneof::Mint {
                    accounts: ConfidentialMintAccounts {
                        token_account: key(0),
                        mint: key(1),
                        instructions_sysvar: proofs.sysvar,
                        equality_context: proofs.equality,
                        ciphertext_validity_context: proofs.ciphertext_validity,
                        range_context: proofs.range,
                        authority: key(proofs.next),
                        multisig_signers: signers(proofs.next + 1),
                    },
                    args: ConfidentialMintArgs {
                        new_decryptable_supply: bytes_of(&data.new_decryptable_supply).to_vec(),
                        mint_amount_auditor_ciphertext_lo: bytes_of(
                            &data.mint_amount_auditor_ciphertext_lo,
                        )
                        .to_vec(),
                        mint_amount_auditor_ciphertext_hi: bytes_of(
                            &data.mint_amount_auditor_ciphertext_hi,
                        )
                        .to_vec(),
                        equality_proof_instruction_offset: data
                            .equality_proof_instruction_offset
                            .into(),
                        ciphertext_validity_proof_instruction_offset: data
                            .ciphertext_validity_proof_instruction_offset
                            .into(),
                        range_proof_instruction_offset: data.range_proof_instruction_offset.into(),
                    },
                })
            },

            SplConfidentialMintBurnInstruction::Burn => {
                check_min_accounts_req(accounts_len, 2)?;

                let data: BurnInstructionData = decode_data(ix, "confidential burn")?;
                let proofs = read_mint_burn_proof_accounts(ix, 2, burn_offsets(&data))?;
                check_min_accounts_req(accounts_len, proofs.next + 1)?;

                oneof::Instruction::Burn(oneof::Burn {
                    accounts: ConfidentialBurnAccounts {
                        token_account: key(0),
                        mint: key(1),
                        instructions_sysvar: proofs.sysvar,
                        equality_context: proofs.equality,
                        ciphertext_validity_context: proofs.ciphertext_validity,
                        range_context: proofs.range,
                        owner: key(proofs.next),
                        multisig_signers: signers(proofs.next + 1),
                    },
                    args: burn_args(&data),
                })
            },

            SplConfidentialMintBurnInstruction::ApplyPendingBurn => {
                check_min_accounts_req(accounts_len, 2)?;

                oneof::Instruction::ApplyPendingBurn(oneof::ApplyPendingBurn {
                    accounts: ApplyPendingBurnAccounts {
                        mint: key(0),
                        authority: key(1),
                        multisig_signers: signers(2),
                    },
                })
            },
        };

        Ok(ConfidentialMintBurnIx {
            instruction: Some(ix_msg),
        })
    }
}

pub(crate) fn burn_offsets(data: &BurnInstructionData) -> [i8; 3] {
    [
        data.equality_proof_instruction_offset,
        data.ciphertext_validity_proof_instruction_offset,
        data.range_proof_instruction_offset,
    ]
}

pub(crate) fn burn_args(data: &BurnInstructionData) -> ConfidentialBurnArgs {
    ConfidentialBurnArgs {
        new_decryptable_available_balance: bytes_of(&data.new_decryptable_available_balance)
            .to_vec(),
        burn_amount_auditor_ciphertext_lo: bytes_of(&data.burn_amount_auditor_ciphertext_lo)
            .to_vec(),
        burn_amount_auditor_ciphertext_hi: bytes_of(&data.burn_amount_auditor_ciphertext_hi)
            .to_vec(),
        equality_proof_instruction_offset: data.equality_proof_instruction_offset.into(),
        ciphertext_validity_proof_instruction_offset: data
            .ciphertext_validity_proof_instruction_offset
            .into(),
        range_proof_instruction_offset: data.range_proof_instruction_offset.into(),
    }
}
