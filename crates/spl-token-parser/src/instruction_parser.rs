use std::sync::Arc;

use shipstern_core::{
    instruction::InstructionUpdate, ParseError, ParseResult, Parser, Prefilter, ProgramParser,
};
use shipstern_parser::{check_min_accounts_req, Error, Result, ResultExt};
use spl_token::instruction::TokenInstruction as SplTokenInstruction;

use crate::Pubkey;

fn pk(key: &shipstern_core::Pubkey) -> Pubkey { Pubkey::new(key.0) }

fn pk_from_key(key: &spl_token::solana_program::pubkey::Pubkey) -> Pubkey {
    Pubkey::new(key.to_bytes())
}

const WITHDRAW_EXCESS_LAMPORTS_TAG: u8 = 38;
const UNWRAP_LAMPORTS_TAG: u8 = 45;
const BATCH_TAG: u8 = 255;
const MAX_BATCH_DEPTH: usize = 8;

fn pks(keys: &[shipstern_core::Pubkey]) -> Vec<Pubkey> { keys.iter().map(pk).collect() }

fn invalid_data(message: &'static str) -> Error { Error::new(message) }

fn unpack_optional_u64(input: &[u8]) -> Result<Option<u64>> {
    match input {
        [] | [0] => Ok(None),
        [1, amount @ ..] if amount.len() == 8 => {
            let amount_bytes: [u8; 8] = amount
                .try_into()
                .map_err(|_| invalid_data("Invalid optional u64 payload"))?;

            Ok(Some(u64::from_le_bytes(amount_bytes)))
        },
        _ => Err(invalid_data("Invalid optional u64 payload")),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InstructionParser;

impl Parser for InstructionParser {
    type Input = InstructionUpdate;
    type Output = crate::TokenProgram;

    fn id(&self) -> std::borrow::Cow<'static, str> { "token_program::InstructionParser".into() }

    fn prefilter(&self) -> Prefilter {
        Prefilter::builder()
            .transaction_accounts_include([spl_token::ID])
            .build()
            .unwrap()
    }

    async fn parse(&self, ix_update: &InstructionUpdate) -> ParseResult<Self::Output> {
        if ix_update.program.equals_ref(spl_token::ID) {
            InstructionParser::parse_impl(ix_update).map_err(|e| ParseError::Other(e.into()))
        } else {
            Err(ParseError::Filtered)
        }
    }
}

impl ProgramParser for InstructionParser {
    #[inline]
    fn program_id(&self) -> shipstern_core::Pubkey { spl_token::ID.to_bytes().into() }
}

#[inline]
fn authority_type_to_proto(a: spl_token::instruction::AuthorityType) -> i32 {
    use spl_token::instruction::AuthorityType as A;

    use crate::AuthorityType as P;

    match a {
        A::MintTokens => P::MintTokens as i32,
        A::FreezeAccount => P::FreezeAccount as i32,
        A::AccountOwner => P::AccountOwner as i32,
        A::CloseAccount => P::CloseAccount as i32,
    }
}

impl InstructionParser {
    #[allow(clippy::too_many_lines)]
    pub fn parse_impl(ix: &InstructionUpdate) -> Result<crate::TokenProgram> {
        Self::parse_impl_with_depth(ix, 0)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_impl_with_depth(
        ix: &InstructionUpdate,
        batch_depth: usize,
    ) -> Result<crate::TokenProgram> {
        let accounts_len = ix.accounts.len();

        let ix_msg = match ix.data.first().copied() {
            Some(WITHDRAW_EXCESS_LAMPORTS_TAG) => {
                check_min_accounts_req(accounts_len, 3)?;

                crate::instruction::Instruction::WithdrawExcessLamports(
                    crate::instruction::WithdrawExcessLamports {
                        accounts: crate::WithdrawExcessLamportsAccounts {
                            source: pk(&ix.accounts[0]),
                            destination: pk(&ix.accounts[1]),
                            authority: pk(&ix.accounts[2]),
                            multisig_signers: pks(&ix.accounts[3..]),
                        },
                    },
                )
            },

            Some(UNWRAP_LAMPORTS_TAG) => {
                check_min_accounts_req(accounts_len, 3)?;

                crate::instruction::Instruction::UnwrapLamports(
                    crate::instruction::UnwrapLamports {
                        accounts: crate::UnwrapLamportsAccounts {
                            source: pk(&ix.accounts[0]),
                            destination: pk(&ix.accounts[1]),
                            authority: pk(&ix.accounts[2]),
                            multisig_signers: pks(&ix.accounts[3..]),
                        },
                        args: crate::UnwrapLamportsArgs {
                            amount: unpack_optional_u64(&ix.data[1..])?,
                        },
                    },
                )
            },

            Some(BATCH_TAG) => Self::parse_batch(ix, batch_depth)?,

            _ => {
                let ix_type = SplTokenInstruction::unpack(&ix.data)
                    .parse_err("Error unpacking token instruction data")?;

                match ix_type {
                    SplTokenInstruction::Transfer { amount } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::Transfer(crate::instruction::Transfer {
                            accounts: crate::TransferAccounts {
                                source: pk(&ix.accounts[0]),
                                destination: pk(&ix.accounts[1]),
                                owner: pk(&ix.accounts[2]),
                                multisig_signers: ix.accounts[3..]
                                    .iter()
                                    .map(|a| Pubkey::new(a.0))
                                    .collect(),
                            },
                            args: crate::TransferArgs { amount },
                        })
                    },

                    SplTokenInstruction::InitializeAccount => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::InitializeAccount(
                            crate::instruction::InitializeAccount {
                                accounts: crate::InitializeAccountAccounts {
                                    account: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                    owner: pk(&ix.accounts[2]),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::InitializeMint {
                        decimals,
                        mint_authority,
                        freeze_authority,
                    }
                    | SplTokenInstruction::InitializeMint2 {
                        decimals,
                        mint_authority,
                        freeze_authority,
                    } => {
                        check_min_accounts_req(accounts_len, 1)?;

                        crate::instruction::Instruction::InitializeMint(
                            crate::instruction::InitializeMint {
                                accounts: crate::InitializeMintAccounts {
                                    mint: pk(&ix.accounts[0]),
                                },
                                args: crate::InitializeMintArgs {
                                    decimals: decimals as u32,
                                    mint_authority: pk_from_key(&mint_authority),
                                    freeze_authority: freeze_authority
                                        .map(|k| Pubkey::new(k.to_bytes()))
                                        .into(),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::InitializeAccount2 { owner } => {
                        check_min_accounts_req(accounts_len, 2)?;

                        crate::instruction::Instruction::InitializeAccount2(
                            crate::instruction::InitializeAccount2 {
                                accounts: crate::InitializeAccount2Accounts {
                                    account: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                },
                                args: crate::InitializeAccount2Args {
                                    owner: pk_from_key(&owner),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::InitializeAccount3 { owner } => {
                        check_min_accounts_req(accounts_len, 2)?;

                        crate::instruction::Instruction::InitializeAccount3(
                            crate::instruction::InitializeAccount3 {
                                accounts: crate::InitializeAccount2Accounts {
                                    account: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                },
                                args: crate::InitializeAccount2Args {
                                    owner: pk_from_key(&owner),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::InitializeMultisig { m } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::InitializeMultisig(
                            crate::instruction::InitializeMultisig {
                                accounts: crate::InitializeMultisigAccounts {
                                    multisig: pk(&ix.accounts[0]),
                                    signers: ix.accounts[2..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::InitializeMultisigArgs { m: m as u32 },
                            },
                        )
                    },

                    SplTokenInstruction::InitializeMultisig2 { m } => {
                        check_min_accounts_req(accounts_len, 2)?;

                        crate::instruction::Instruction::InitializeMultisig(
                            crate::instruction::InitializeMultisig {
                                accounts: crate::InitializeMultisigAccounts {
                                    multisig: pk(&ix.accounts[0]),
                                    signers: ix.accounts[1..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::InitializeMultisigArgs { m: m as u32 },
                            },
                        )
                    },

                    SplTokenInstruction::Approve { amount } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::Approve(crate::instruction::Approve {
                            accounts: crate::ApproveAccounts {
                                source: pk(&ix.accounts[0]),
                                delegate: pk(&ix.accounts[1]),
                                owner: pk(&ix.accounts[2]),
                                multisig_signers: ix.accounts[3..]
                                    .iter()
                                    .map(|a| Pubkey::new(a.0))
                                    .collect(),
                            },
                            args: crate::ApproveArgs { amount },
                        })
                    },

                    SplTokenInstruction::Revoke => {
                        check_min_accounts_req(accounts_len, 2)?;

                        crate::instruction::Instruction::Revoke(crate::instruction::Revoke {
                            accounts: crate::RevokeAccounts {
                                source: pk(&ix.accounts[0]),
                                owner: pk(&ix.accounts[1]),
                                multisig_signers: ix.accounts[2..]
                                    .iter()
                                    .map(|a| Pubkey::new(a.0))
                                    .collect(),
                            },
                        })
                    },

                    SplTokenInstruction::SetAuthority {
                        authority_type,
                        new_authority,
                    } => {
                        check_min_accounts_req(accounts_len, 2)?;

                        crate::instruction::Instruction::SetAuthority(
                            crate::instruction::SetAuthority {
                                accounts: crate::SetAuthorityAccounts {
                                    account: pk(&ix.accounts[0]),
                                    current_authority: pk(&ix.accounts[1]),
                                    multisig_signers: ix.accounts[2..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::SetAuthorityArgs {
                                    authority_type: authority_type_to_proto(authority_type),
                                    new_authority: new_authority
                                        .map(|k| Pubkey::new(k.to_bytes()))
                                        .into(),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::MintTo { amount } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::MintTo(crate::instruction::MintTo {
                            accounts: crate::MintToAccounts {
                                mint: pk(&ix.accounts[0]),
                                account: pk(&ix.accounts[1]),
                                mint_authority: pk(&ix.accounts[2]),
                                multisig_signers: ix.accounts[3..]
                                    .iter()
                                    .map(|a| Pubkey::new(a.0))
                                    .collect(),
                            },
                            args: crate::MintToArgs { amount },
                        })
                    },

                    SplTokenInstruction::Burn { amount } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::Burn(crate::instruction::Burn {
                            accounts: crate::BurnAccounts {
                                account: pk(&ix.accounts[0]),
                                mint: pk(&ix.accounts[1]),
                                owner: pk(&ix.accounts[2]),
                                multisig_signers: ix.accounts[3..]
                                    .iter()
                                    .map(|a| Pubkey::new(a.0))
                                    .collect(),
                            },
                            args: crate::BurnArgs { amount },
                        })
                    },

                    SplTokenInstruction::CloseAccount => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::CloseAccount(
                            crate::instruction::CloseAccount {
                                accounts: crate::CloseAccountAccounts {
                                    account: pk(&ix.accounts[0]),
                                    destination: pk(&ix.accounts[1]),
                                    owner: pk(&ix.accounts[2]),
                                    multisig_signers: ix.accounts[3..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::FreezeAccount => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::FreezeAccount(
                            crate::instruction::FreezeAccount {
                                accounts: crate::FreezeAccountAccounts {
                                    account: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                    mint_freeze_authority: pk(&ix.accounts[2]),
                                    multisig_signers: ix.accounts[3..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::ThawAccount => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::ThawAccount(
                            crate::instruction::ThawAccount {
                                accounts: crate::ThawAccountAccounts {
                                    account: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                    mint_freeze_authority: pk(&ix.accounts[2]),
                                    multisig_signers: ix.accounts[3..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::TransferChecked { amount, decimals } => {
                        check_min_accounts_req(accounts_len, 4)?;

                        crate::instruction::Instruction::TransferChecked(
                            crate::instruction::TransferChecked {
                                accounts: crate::TransferCheckedAccounts {
                                    source: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                    destination: pk(&ix.accounts[2]),
                                    owner: pk(&ix.accounts[3]),
                                    multisig_signers: ix.accounts[4..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::TransferCheckedArgs {
                                    amount,
                                    decimals: decimals as u32,
                                },
                            },
                        )
                    },

                    SplTokenInstruction::ApproveChecked { amount, decimals } => {
                        check_min_accounts_req(accounts_len, 4)?;

                        crate::instruction::Instruction::ApproveChecked(
                            crate::instruction::ApproveChecked {
                                accounts: crate::ApproveCheckedAccounts {
                                    source: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                    delegate: pk(&ix.accounts[2]),
                                    owner: pk(&ix.accounts[3]),
                                    multisig_signers: ix.accounts[4..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::ApproveCheckedArgs {
                                    amount,
                                    decimals: decimals as u32,
                                },
                            },
                        )
                    },

                    SplTokenInstruction::MintToChecked { amount, decimals } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::MintToChecked(
                            crate::instruction::MintToChecked {
                                accounts: crate::MintToCheckedAccounts {
                                    mint: pk(&ix.accounts[0]),
                                    account: pk(&ix.accounts[1]),
                                    mint_authority: pk(&ix.accounts[2]),
                                    multisig_signers: ix.accounts[3..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::MintToCheckedArgs {
                                    amount,
                                    decimals: decimals as u32,
                                },
                            },
                        )
                    },

                    SplTokenInstruction::BurnChecked { amount, decimals } => {
                        check_min_accounts_req(accounts_len, 3)?;

                        crate::instruction::Instruction::BurnChecked(
                            crate::instruction::BurnChecked {
                                accounts: crate::BurnCheckedAccounts {
                                    account: pk(&ix.accounts[0]),
                                    mint: pk(&ix.accounts[1]),
                                    owner: pk(&ix.accounts[2]),
                                    multisig_signers: ix.accounts[3..]
                                        .iter()
                                        .map(|a| Pubkey::new(a.0))
                                        .collect(),
                                },
                                args: crate::BurnCheckedArgs {
                                    amount,
                                    decimals: decimals as u32,
                                },
                            },
                        )
                    },

                    SplTokenInstruction::SyncNative => {
                        check_min_accounts_req(accounts_len, 1)?;

                        crate::instruction::Instruction::SyncNative(
                            crate::instruction::SyncNative {
                                accounts: crate::SyncNativeAccounts {
                                    account: pk(&ix.accounts[0]),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::GetAccountDataSize => {
                        check_min_accounts_req(accounts_len, 1)?;

                        crate::instruction::Instruction::GetAccountDataSize(
                            crate::instruction::GetAccountDataSize {
                                accounts: crate::GetAccountDataSizeAccounts {
                                    mint: pk(&ix.accounts[0]),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::InitializeImmutableOwner => {
                        check_min_accounts_req(accounts_len, 1)?;

                        crate::instruction::Instruction::InitializeImmutableOwner(
                            crate::instruction::InitializeImmutableOwner {
                                accounts: crate::InitializeImmutableOwnerAccounts {
                                    account: pk(&ix.accounts[0]),
                                },
                            },
                        )
                    },

                    SplTokenInstruction::AmountToUiAmount { amount } => {
                        check_min_accounts_req(accounts_len, 1)?;

                        crate::instruction::Instruction::AmountToUiAmount(
                            crate::instruction::AmountToUiAmount {
                                accounts: crate::AmountToUiAmountAccounts {
                                    mint: pk(&ix.accounts[0]),
                                },
                                args: crate::AmountToUiAmountArgs { amount },
                            },
                        )
                    },

                    SplTokenInstruction::UiAmountToAmount { ui_amount } => {
                        check_min_accounts_req(accounts_len, 1)?;

                        crate::instruction::Instruction::UiAmountToAmount(
                            crate::instruction::UiAmountToAmount {
                                accounts: crate::UiAmountToAmountAccounts {
                                    mint: pk(&ix.accounts[0]),
                                },
                                args: crate::UiAmountToAmountArgs {
                                    ui_amount: ui_amount.into(),
                                },
                            },
                        )
                    },
                }
            },
        };

        Ok(crate::TokenProgram {
            instruction: Some(ix_msg),
        })
    }

    fn parse_batch(
        ix: &InstructionUpdate,
        batch_depth: usize,
    ) -> Result<crate::instruction::Instruction> {
        if batch_depth >= MAX_BATCH_DEPTH {
            return Err(invalid_data("Maximum p-token batch nesting exceeded"));
        }

        let mut cursor = 1;
        let mut account_offset = 0usize;
        let mut instructions = Vec::new();

        while cursor < ix.data.len() {
            let number_of_accounts = usize::from(
                *ix.data
                    .get(cursor)
                    .ok_or_else(|| invalid_data("Missing p-token batch account count"))?,
            );
            cursor += 1;

            let instruction_data_len = usize::from(
                *ix.data
                    .get(cursor)
                    .ok_or_else(|| invalid_data("Missing p-token batch instruction length"))?,
            );
            cursor += 1;

            let next_cursor = cursor
                .checked_add(instruction_data_len)
                .ok_or_else(|| invalid_data("Invalid p-token batch instruction length"))?;
            let instruction_data = ix
                .data
                .get(cursor..next_cursor)
                .ok_or_else(|| invalid_data("Truncated p-token batch instruction data"))?;
            cursor = next_cursor;

            let account_end = account_offset
                .checked_add(number_of_accounts)
                .ok_or_else(|| invalid_data("Invalid p-token batch account count"))?;
            let accounts = ix
                .accounts
                .get(account_offset..account_end)
                .ok_or_else(|| invalid_data("Too few accounts for p-token batch entry"))?;
            account_offset = account_end;
            let number_of_accounts = u32::try_from(number_of_accounts)
                .map_err(|_| invalid_data("Too many accounts for p-token batch entry"))?;

            let batch_index = instructions
                .len()
                .try_into()
                .map_err(|_| invalid_data("Too many p-token batch entries"))?;
            let inner_ix = InstructionUpdate {
                program: ix.program,
                accounts: accounts.to_vec(),
                data: instruction_data.to_vec(),
                shared: Arc::clone(&ix.shared),
                inner: Vec::new(),
                path: ix.path.push_clone(batch_index),
                log_range: ix.log_range.clone(),
            };

            let parsed_instruction = Self::parse_impl_with_depth(&inner_ix, batch_depth + 1).ok();

            instructions.push(crate::BatchInstruction {
                number_of_accounts,
                accounts: pks(accounts),
                data: instruction_data.to_vec(),
                instruction: parsed_instruction,
            });
        }

        Ok(crate::instruction::Instruction::Batch(crate::Batch {
            instructions,
            remaining_accounts: pks(&ix.accounts[account_offset..]),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Mul;

    use shipstern_mock::tx_fixture;

    use super::*;

    const PTOKEN_BATCH_TX: &str =
        "5cqt7QpW2WhyQoJC7u8WhVivoUC5rWrHudWAssYrm2xDTTvVszVAXyCidco5DAXqwc4u4DSAXmmt1kzCKwJP2iW8";
    const PTOKEN_WITHDRAW_EXCESS_LAMPORTS_TX: &str =
        "3YLwc2z7pbzU1xbduwxkjjMcdzyPE7ozoVH5jrMMDzNEK7zUgdMhbtBwE17gJcgBNBKvNxWrCS76qzfjpabXgwoL";
    const PTOKEN_UNWRAP_LAMPORTS_TX: &str =
        "3kHqY7UUDsyqYoyhSBNjpQjWCr8BhZB6eT7MDzU2ThwECuDdRHm2FjUktikf6crHLmHo5WWuEBdL4caZec5YbFTg";

    #[tokio::test]
    async fn test_mint_to_checked_ix_parsing() {
        let parser = InstructionParser;

        let ixs =
            tx_fixture!("55kpnRufcX9Fo44oRBXtrkxPRww4UWJKxCpgBV39kzAAag8oyJbd9Y3YWdQQUi3TBqtrhjgsMGb9Nw8bUxy7j5rt", &parser);

        let ix0 = &ixs[0];

        let Some(crate::instruction::Instruction::MintToChecked(mtc)) =
            ix0.as_ref().and_then(|i| i.instruction.as_ref())
        else {
            panic!("Invalid Instruction");
        };

        let args = &mtc.args;

        assert_eq!(args.decimals, 10);
        assert_eq!(args.amount, 10.mul(10u64.pow(args.decimals)));
    }

    #[tokio::test]
    async fn test_ptoken_batch_ix_parsing() {
        let parser = InstructionParser;

        let ixs = tx_fixture!(PTOKEN_BATCH_TX, &parser);

        let actual =
            ixs.iter()
                .filter_map(Option::as_ref)
                .find_map(|ix| match ix.instruction.as_ref() {
                    Some(crate::instruction::Instruction::Batch(batch)) => {
                        let batch_ix = batch.instructions.first()?;
                        let Some(crate::TokenProgram {
                            instruction: Some(crate::instruction::Instruction::MintTo(mint_to)),
                        }) = &batch_ix.instruction
                        else {
                            return None;
                        };

                        Some((
                            batch.instructions.len(),
                            batch.remaining_accounts.len(),
                            batch_ix.number_of_accounts,
                            batch_ix.data.clone(),
                            mint_to.args.amount,
                            mint_to.accounts.multisig_signers.len(),
                        ))
                    },
                    _ => None,
                });

        assert_eq!(
            actual,
            Some((1, 0, 3, vec![7, 0, 171, 135, 4, 0, 0, 0, 0], 76_000_000, 0)),
            "p-token batch fixture should wrap a single MintTo instruction",
        );
    }

    #[tokio::test]
    async fn test_ptoken_unwrap_lamports_ix_parsing() {
        let parser = InstructionParser;

        let ixs = tx_fixture!(PTOKEN_UNWRAP_LAMPORTS_TX, &parser);

        let actual =
            ixs.iter()
                .filter_map(Option::as_ref)
                .find_map(|ix| match ix.instruction.as_ref() {
                    Some(crate::instruction::Instruction::UnwrapLamports(unwrap_lamports)) => {
                        Some((
                            unwrap_lamports.accounts.source.to_string(),
                            unwrap_lamports.accounts.destination.to_string(),
                            unwrap_lamports.accounts.authority.to_string(),
                            unwrap_lamports.accounts.multisig_signers.len(),
                            unwrap_lamports.args.amount,
                        ))
                    },
                    _ => None,
                });

        assert_eq!(
            actual,
            Some((
                "8gFZKWfWHcaLekuCoe7qNY8qdLwRKvsdRxU4zMHsBsMR".to_string(),
                "5K82iU97sj2F7hERTHy86HfdPbBqw3bJn9QkrN5tJ1vj".to_string(),
                "F7p3dFrjRTbtRp8FRF6qHLomXbKRBzpvBLjtQcfcgmNe".to_string(),
                0,
                Some(2_752_920_000),
            )),
            "p-token unwrap fixture should decode source, destination, authority, and amount",
        );
    }

    #[tokio::test]
    async fn test_ptoken_withdraw_excess_lamports_ix_parsing() {
        let parser = InstructionParser;

        let ixs = tx_fixture!(PTOKEN_WITHDRAW_EXCESS_LAMPORTS_TX, &parser);

        let actual =
            ixs.iter()
                .filter_map(Option::as_ref)
                .find_map(|ix| match ix.instruction.as_ref() {
                    Some(crate::instruction::Instruction::WithdrawExcessLamports(
                        withdraw_excess_lamports,
                    )) => Some((
                        withdraw_excess_lamports.accounts.source.to_string(),
                        withdraw_excess_lamports.accounts.destination.to_string(),
                        withdraw_excess_lamports.accounts.authority.to_string(),
                        withdraw_excess_lamports.accounts.multisig_signers.len(),
                    )),
                    _ => None,
                });

        assert_eq!(
            actual,
            Some((
                "41niYdTfJAqJHPfxkhm4LDYpYsUaSqhcMUbfYND9sDmL".to_string(),
                "CqJVUVbxJae8GfYsSooA5qzjHmoZusB1Hni7Ed1eEDeH".to_string(),
                "CqJVUVbxJae8GfYsSooA5qzjHmoZusB1Hni7Ed1eEDeH".to_string(),
                0,
            )),
            "p-token withdraw excess lamports fixture should decode source, destination, and \
             authority",
        );
    }

    #[cfg(feature = "proto")]
    #[tokio::test]
    async fn test_ptoken_batch_proto_round_trip() {
        use prost::Message as _;

        let parser = InstructionParser;

        let ixs = tx_fixture!(PTOKEN_BATCH_TX, &parser);

        let original = ixs
            .into_iter()
            .flatten()
            .find(|ix| {
                matches!(
                    ix.instruction.as_ref(),
                    Some(crate::instruction::Instruction::Batch(_))
                )
            })
            .expect("no p-token batch instruction in fixture");
        let mut buf = Vec::new();
        original.encode(&mut buf).expect("proto encode failed");

        let decoded = crate::TokenProgram::decode(buf.as_slice()).expect("proto decode failed");

        assert_eq!(decoded, original);
    }
}
