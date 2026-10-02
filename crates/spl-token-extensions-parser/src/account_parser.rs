use std::borrow::Cow;

use shipstern_core::{AccountUpdate, ParseResult, Parser, Prefilter, ProgramParser};
use shipstern_proc_macro::shipstern;
use solana_program_error::ProgramError;
use solana_program_pack::Pack;
use spl_token_2022::{
    extension::{BaseStateWithExtensions, StateWithExtensions},
    state::{Account as SplAccount, AccountState, Mint as SplMint, Multisig as SplMultisig},
};

use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct TokenExtensionState {
    #[hint(oneof = "account::Account", tags = "1, 2, 3")]
    pub account: Option<account::Account>,
}

pub mod account {
    use super::shipstern;

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Account {
        ExtendedTokenAccount(super::ExtendedTokenAccount),
        ExtendedMint(super::ExtendedMint),
        Multisig(super::Multisig),
    }
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ExtensionData {
    /// `spl_token_2022::extension::ExtensionType` as i32
    pub extension_type: i32,

    /// Raw bytes of the extension payload (exactly what Token-2022 stores)
    pub data: Vec<u8>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ExtendedMint {
    pub base_account: Option<Mint>,
    pub extensions: Vec<ExtensionData>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ExtendedTokenAccount {
    pub base_account: Option<Account>,
    pub extensions: Vec<ExtensionData>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct Mint {
    pub mint_authority: Option<Pubkey>,
    pub supply: u64,
    pub decimals: u32,
    pub is_initialized: bool,
    pub freeze_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct Account {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
    pub delegate: Option<Pubkey>,
    /// `spl_token_2022::state::AccountState` as u32
    pub state: u32,
    /// If native: rent-exempt reserve lamports (same semantics as spl-token)
    pub is_native: Option<u64>,
    pub delegated_amount: u64,
    pub close_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct Multisig {
    pub m: u32,
    pub n: u32,
    pub is_initialized: bool,
    pub signers: Vec<Pubkey>,
}

fn spl_to_mint(m: &SplMint) -> Mint {
    Mint {
        mint_authority: m.mint_authority.map(|pk| Pubkey::new(pk.to_bytes())).into(),
        supply: m.supply,
        decimals: m.decimals as u32,
        is_initialized: m.is_initialized,
        freeze_authority: m
            .freeze_authority
            .map(|pk| Pubkey::new(pk.to_bytes()))
            .into(),
    }
}

fn account_state_to_u32(s: AccountState) -> u32 {
    match s {
        AccountState::Uninitialized => 0,
        AccountState::Initialized => 1,
        AccountState::Frozen => 2,
    }
}

fn spl_to_account(a: &SplAccount) -> Account {
    Account {
        mint: Pubkey::new(a.mint.to_bytes()),
        owner: Pubkey::new(a.owner.to_bytes()),
        amount: a.amount,
        delegate: a.delegate.map(|pk| Pubkey::new(pk.to_bytes())).into(),
        state: account_state_to_u32(a.state),
        is_native: a.is_native.into(),
        delegated_amount: a.delegated_amount,
        close_authority: a
            .close_authority
            .map(|pk| Pubkey::new(pk.to_bytes()))
            .into(),
    }
}

fn spl_to_multisig(multisig: &SplMultisig) -> Multisig {
    // Multisig has fixed signers array; keep only the first `n` signers
    let n = multisig.n as usize;
    let max = multisig.signers.len().min(n);

    let mut signers = Vec::with_capacity(max);

    for i in 0..max {
        signers.push(Pubkey::new(multisig.signers[i].to_bytes()));
    }

    Multisig {
        m: multisig.m as u32,
        n: multisig.n as u32,
        is_initialized: multisig.is_initialized,
        signers,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenExtensionAccountType {
    TokenAccount,
    Mint,
    Multisig,
}

fn extension_account_type(data_bytes: &[u8]) -> Result<TokenExtensionAccountType, ProgramError> {
    if StateWithExtensions::<SplMint>::unpack(data_bytes).is_ok() {
        return Ok(TokenExtensionAccountType::Mint);
    }

    if StateWithExtensions::<SplAccount>::unpack(data_bytes).is_ok() {
        return Ok(TokenExtensionAccountType::TokenAccount);
    }

    if SplMultisig::unpack(data_bytes).is_ok() {
        return Ok(TokenExtensionAccountType::Multisig);
    }

    Err(ProgramError::InvalidAccountData)
}

fn build_extensions_for_mint(
    unpacked: &StateWithExtensions<SplMint>,
) -> Result<Vec<ExtensionData>, ProgramError> {
    let extension_types = unpacked.get_extension_types()?;

    let mut out = Vec::with_capacity(extension_types.len());

    for ext in extension_types {
        let data = crate::accounts::mint_account_extensions_data_bytes(unpacked, ext)?;

        out.push(ExtensionData {
            extension_type: ext as i32,
            data: data.to_vec(),
        });
    }

    Ok(out)
}

fn build_extensions_for_account(
    unpacked: &StateWithExtensions<SplAccount>,
) -> Result<Vec<ExtensionData>, ProgramError> {
    let extension_types = unpacked.get_extension_types()?;

    let mut out = Vec::with_capacity(extension_types.len());

    for ext in extension_types {
        let data = crate::accounts::token_account_extensions_data_bytes(unpacked, ext)?;

        out.push(ExtensionData {
            extension_type: ext as i32,
            data: data.to_vec(),
        });
    }

    Ok(out)
}

impl TokenExtensionState {
    pub fn try_unpack(data_bytes: &[u8]) -> ParseResult<Self> {
        match extension_account_type(data_bytes)? {
            TokenExtensionAccountType::Mint => {
                let unpacked = StateWithExtensions::<SplMint>::unpack(data_bytes)?;
                let extensions = build_extensions_for_mint(&unpacked)?;

                Ok(TokenExtensionState {
                    account: Some(account::Account::ExtendedMint(ExtendedMint {
                        base_account: Some(spl_to_mint(&unpacked.base)),
                        extensions,
                    })),
                })
            },
            TokenExtensionAccountType::TokenAccount => {
                let unpacked = StateWithExtensions::<SplAccount>::unpack(data_bytes)?;
                let extensions = build_extensions_for_account(&unpacked)?;

                Ok(TokenExtensionState {
                    account: Some(account::Account::ExtendedTokenAccount(
                        ExtendedTokenAccount {
                            base_account: Some(spl_to_account(&unpacked.base)),
                            extensions,
                        },
                    )),
                })
            },
            TokenExtensionAccountType::Multisig => {
                let multisig = SplMultisig::unpack(data_bytes)?;

                Ok(TokenExtensionState {
                    account: Some(account::Account::Multisig(spl_to_multisig(&multisig))),
                })
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AccountParser;

impl Parser for AccountParser {
    type Input = AccountUpdate;
    type Output = TokenExtensionState;

    fn id(&self) -> Cow<'static, str> { "token_extensions::AccountParser".into() }

    fn prefilter(&self) -> Prefilter {
        Prefilter::builder()
            .account_owners([spl_token_2022::ID])
            .build()
            .unwrap()
    }

    async fn parse(&self, acct: &AccountUpdate) -> ParseResult<Self::Output> {
        let inner = acct.account.as_ref().ok_or(ProgramError::InvalidArgument)?;
        TokenExtensionState::try_unpack(&inner.data)
    }
}

impl ProgramParser for AccountParser {
    #[inline]
    fn program_id(&self) -> shipstern_core::Pubkey { spl_token_2022::ID.to_bytes().into() }
}

#[cfg(test)]
mod tests {
    use core::panic;

    use shipstern_core::Parser;
    use shipstern_mock::account_fixture;

    use super::{account, AccountParser, TokenExtensionState};

    #[tokio::test]
    async fn test_mint_account_parsing_proto() {
        let parser = AccountParser;

        let account = account_fixture!("BtSLwAFDsMX4bhamtyggn2xsdFKQvpaSzw9jEL7BNuyu", &parser);

        let state = account.account.expect("missing account");

        let account::Account::ExtendedMint(ext_mint) = state else {
            panic!("Invalid Account");
        };

        let base = ext_mint.base_account.expect("missing base mint");

        assert_eq!(base.decimals, 9);

        // Extensions count will depend on the fixture
        assert!(!ext_mint.extensions.is_empty());
    }

    /// `PermissionedBurn` is new in spl-token-2022 11. No mainnet account uses it
    /// (0 of 3851 with extensions), so the mint is built with Token-2022's writer.
    #[test]
    fn permissioned_burn_mint_parses_into_an_extension_entry() {
        use spl_token_2022::{
            extension::{
                permissioned_burn::PermissionedBurnConfig, BaseStateWithExtensionsMut,
                ExtensionType, PodStateWithExtensionsMut,
            },
            pod::PodMint,
        };

        let len =
            ExtensionType::try_calculate_account_len::<PodMint>(&[ExtensionType::PermissionedBurn])
                .expect("account len");
        let mut buf = vec![0u8; len];

        {
            let mut state = PodStateWithExtensionsMut::<PodMint>::unpack_uninitialized(&mut buf)
                .expect("uninitialized mint");
            let ext = state
                .init_extension::<PermissionedBurnConfig>(true)
                .expect("init PermissionedBurn");
            ext.authority = Some(solana_pubkey::Pubkey::new_from_array([7u8; 32]))
                .try_into()
                .expect("authority");
            state.base.decimals = 6;
            state.base.is_initialized = true.into();
            state.init_account_type().expect("account type");
        }

        let state = TokenExtensionState::try_unpack(&buf).expect("mint should unpack");

        let Some(account::Account::ExtendedMint(mint)) = state.account else {
            panic!("expected an extended mint")
        };

        let ext = mint
            .extensions
            .iter()
            .find(|e| e.extension_type == ExtensionType::PermissionedBurn as i32)
            .expect("PermissionedBurn must survive parsing");

        // `PermissionedBurnConfig` is a single `MaybeNull<Address>`, so the
        // payload is exactly the 32 authority bytes. Comparing them rules out
        // a parser that keeps the type tag but hands back the wrong slice.
        assert_eq!(ext.data, vec![7u8; 32], "authority bytes must round-trip");
    }
}
