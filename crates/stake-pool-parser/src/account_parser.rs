use shipstern_core::{AccountUpdate, ParseError, ParseResult, Parser, Prefilter, ProgramParser};
use shipstern_proc_macro::shipstern;
use solana_borsh::v1::try_from_slice_unchecked;
use solana_program_error::ProgramError;
use spl_stake_pool::state::{StakePool, ValidatorList};

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SplStakePoolProgramState {
    #[hint(oneof = "spl_stake_pool_program_state::State", tags = "1, 2")]
    pub state: Option<spl_stake_pool_program_state::State>,
}

pub mod spl_stake_pool_program_state {
    use super::shipstern;

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum State {
        StakePool(super::StakePoolAccount),
        ValidatorList(super::ValidatorListAccount),
    }
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct StakePoolAccount {
    /// First byte discriminator (1)
    pub account_type: u32,

    /// Raw borsh bytes (including discriminator byte)
    pub data: Vec<u8>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ValidatorListAccount {
    /// First byte discriminator (2)
    pub account_type: u32,

    /// Raw borsh bytes (including discriminator byte)
    pub data: Vec<u8>,
}

#[allow(clippy::large_enum_variant)]
pub enum NativeStakePoolState {
    StakePool(StakePool),
    ValidatorList(ValidatorList),
}

impl NativeStakePoolState {
    pub fn try_unpack(data_bytes: &[u8]) -> ParseResult<Self> {
        let first_byte = *data_bytes
            .first()
            .ok_or_else(|| ParseError::from("Empty account data".to_owned()))?;

        match first_byte {
            0 => Err(ParseError::from("Uninitialized Account".to_owned())),
            1 => {
                let stake_pool = try_from_slice_unchecked::<StakePool>(data_bytes)?;
                Ok(NativeStakePoolState::StakePool(stake_pool))
            },
            2 => {
                let validator_list = try_from_slice_unchecked::<ValidatorList>(data_bytes)?;
                Ok(NativeStakePoolState::ValidatorList(validator_list))
            },
            _ => Err(ParseError::from("Invalid Account".to_owned())),
        }
    }

    pub fn try_unpack_proto(data_bytes: &[u8]) -> ParseResult<SplStakePoolProgramState> {
        let first_byte = *data_bytes
            .first()
            .ok_or_else(|| ParseError::from("Empty account data".to_owned()))?;

        match first_byte {
            0 => Err(ParseError::from("Uninitialized Account".to_owned())),
            1 => Ok(SplStakePoolProgramState {
                state: Some(spl_stake_pool_program_state::State::StakePool(
                    StakePoolAccount {
                        account_type: 1,
                        data: data_bytes.to_vec(),
                    },
                )),
            }),
            2 => Ok(SplStakePoolProgramState {
                state: Some(spl_stake_pool_program_state::State::ValidatorList(
                    ValidatorListAccount {
                        account_type: 2,
                        data: data_bytes.to_vec(),
                    },
                )),
            }),
            _ => Err(ParseError::from("Invalid Account".to_owned())),
        }
    }
}

#[derive(Copy, Clone)]
pub struct AccountParser;

impl Parser for AccountParser {
    type Input = AccountUpdate;
    type Output = SplStakePoolProgramState;

    fn id(&self) -> std::borrow::Cow<'static, str> { "spl_stake_pool::AccountParser".into() }

    fn prefilter(&self) -> Prefilter {
        Prefilter::builder()
            .account_owners([spl_stake_pool::ID])
            .build()
            .unwrap()
    }

    async fn parse(&self, acct: &AccountUpdate) -> ParseResult<Self::Output> {
        let inner = acct.account.as_ref().ok_or(ProgramError::InvalidArgument)?;
        NativeStakePoolState::try_unpack_proto(&inner.data)
    }
}

impl ProgramParser for AccountParser {
    #[inline]
    fn program_id(&self) -> shipstern_core::Pubkey { spl_stake_pool::ID.to_bytes().into() }
}
