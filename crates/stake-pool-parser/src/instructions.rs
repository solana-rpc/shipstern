use shipstern_proc_macro::shipstern;

use crate::Pubkey;

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct StakePoolProgram {
    #[hint(
        oneof = "stake_pool_program::Instruction",
        tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, \
                23, 24, 25, 26"
    )]
    pub instruction: Option<stake_pool_program::Instruction>,
}

pub mod stake_pool_program {
    use super::shipstern;

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        Initialize(super::InitializeInstruction),
        AddValidatorToPool(super::AddValidatorToPoolInstruction),
        RemoveValidatorFromPool(super::RemoveValidatorFromPoolInstruction),
        DecreaseValidatorStake(super::DecreaseValidatorStakeInstruction),
        IncreaseValidatorStake(super::IncreaseValidatorStakeInstruction),
        SetPreferredValidator(super::SetPreferredValidatorInstruction),
        UpdateValidatorListBalance(super::UpdateValidatorListBalanceInstruction),
        UpdateStakePoolBalance(super::UpdateStakePoolBalanceInstruction),
        CleanupRemovedValidatorEntries(super::CleanupRemovedValidatorEntriesInstruction),
        DepositStake(super::DepositStakeInstruction),
        WithdrawStake(super::WithdrawStakeInstruction),
        SetManager(super::SetManagerInstruction),
        SetFee(super::SetFeeInstruction),
        SetStaker(super::SetStakerInstruction),
        DepositSol(super::DepositSolInstruction),
        SetFundingAuthority(super::SetFundingAuthorityInstruction),
        WithdrawSol(super::WithdrawSolInstruction),
        CreateTokenMetadata(super::CreateTokenMetadataInstruction),
        UpdateTokenMetadata(super::UpdateTokenMetadataInstruction),
        IncreaseAdditionalValidatorStake(super::IncreaseAdditionalValidatorStakeInstruction),
        DecreaseAdditionalValidatorStake(super::DecreaseAdditionalValidatorStakeInstruction),
        DecreaseValidatorStakeWithReserve(super::DecreaseValidatorStakeWithReserveInstruction),
        DepositStakeWithSlippage(super::DepositStakeWithSlippageInstruction),
        WithdrawStakeWithSlippage(super::WithdrawStakeWithSlippageInstruction),
        DepositSolWithSlippage(super::DepositSolWithSlippageInstruction),
        WithdrawSolWithSlippage(super::WithdrawSolWithSlippageInstruction),
    }
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct Fee {
    pub numerator: u64,
    pub denominator: u64,
}

#[shipstern(enumeration)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum PreferredValidatorType {
    Deposit = 0,
    Withdraw = 1,
}

#[shipstern(enumeration)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum FundingType {
    SolDeposit = 0,
    StakeDeposit = 1,
    SolWithdraw = 2,
}

#[shipstern(enumeration)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(i32)]
pub enum FeeKind {
    SolReferral = 0,
    StakeReferral = 1,
    Epoch = 2,
    StakeWithdrawal = 3,
    SolDeposit = 4,
    StakeDeposit = 5,
    SolWithdrawal = 6,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct FeeType {
    #[hint(enumeration = "FeeKind")]
    pub kind: i32,

    #[hint(oneof = "fee_type::Value", tags = "2, 3")]
    pub value: Option<fee_type::Value>,
}

pub mod fee_type {
    use super::shipstern;

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Value {
        Fee(super::Fee),

        ReferralBps(u32),
    }
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeAccounts {
    pub stake_pool: Pubkey,
    pub manager: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list: Pubkey,
    pub reserve_stake: Pubkey,
    pub pool_mint: Pubkey,
    pub manager_pool_account: Pubkey,
    pub token_program: Pubkey,
    pub deposit_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeArgs {
    pub fee: Option<Fee>,
    pub withdrawal_fee: Option<Fee>,
    pub deposit_fee: Option<Fee>,
    pub referral_fee: u32,
    pub max_validators: u32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeInstruction {
    pub accounts: InitializeAccounts,
    pub args: InitializeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct AddValidatorToPoolAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub funder: Pubkey,
    pub stake_pool_withdraw: Pubkey,
    pub validator_list: Pubkey,
    pub stake: Pubkey,
    pub validator: Pubkey,
    pub rent: Pubkey,
    pub clock: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub stake_config: Pubkey,
    pub system_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct AddValidatorToPoolArgs {
    pub raw_validator_seed: u32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct AddValidatorToPoolInstruction {
    pub accounts: AddValidatorToPoolAccounts,
    pub args: AddValidatorToPoolArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct RemoveValidatorFromPoolAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw: Pubkey,
    pub validator_list: Pubkey,
    pub stake_account: Pubkey,
    pub transient_stake_account: Pubkey,
    pub clock: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct RemoveValidatorFromPoolInstruction {
    pub accounts: RemoveValidatorFromPoolAccounts,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseValidatorStakeAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list: Pubkey,
    pub validator_stake: Pubkey,
    pub transient_stake: Pubkey,
    pub clock: Pubkey,
    pub rent: Pubkey,
    pub system_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseValidatorStakeArgs {
    pub lamports: u64,
    pub transient_stake_seed: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseValidatorStakeInstruction {
    pub accounts: DecreaseValidatorStakeAccounts,
    pub args: DecreaseValidatorStakeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct IncreaseValidatorStakeAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list: Pubkey,
    pub reserve_stake: Pubkey,
    pub transient_stake: Pubkey,
    pub validator_stake: Pubkey,
    pub validator: Pubkey,
    pub clock: Pubkey,
    pub rent: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub stake_config: Pubkey,
    pub system_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct IncreaseValidatorStakeArgs {
    pub lamports: u64,
    pub transient_stake_seed: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct IncreaseValidatorStakeInstruction {
    pub accounts: IncreaseValidatorStakeAccounts,
    pub args: IncreaseValidatorStakeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetPreferredValidatorAccounts {
    pub stake_pool_address: Pubkey,
    pub staker: Pubkey,
    pub validator_list_address: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetPreferredValidatorArgs {
    #[hint(enumeration = "PreferredValidatorType")]
    pub validator_type: i32,
    pub validator_vote_address: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetPreferredValidatorInstruction {
    pub accounts: SetPreferredValidatorAccounts,
    pub args: SetPreferredValidatorArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateValidatorListBalanceAccounts {
    pub stake_pool: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list_address: Pubkey,
    pub reserve_stake: Pubkey,
    pub clock: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub stake_program: Pubkey,
    pub validator_stake_accounts: Vec<Pubkey>,
    pub transient_stake_accounts: Vec<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateValidatorListBalanceArgs {
    pub start_index: u32,
    pub no_merge: bool,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateValidatorListBalanceInstruction {
    pub accounts: UpdateValidatorListBalanceAccounts,
    pub args: UpdateValidatorListBalanceArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateStakePoolBalanceAccounts {
    pub stake_pool: Pubkey,
    pub withdraw_authority: Pubkey,
    pub validator_list_storage: Pubkey,
    pub reserve_stake: Pubkey,
    pub manager_fee_account: Pubkey,
    pub stake_pool_mint: Pubkey,
    pub token_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateStakePoolBalanceInstruction {
    pub accounts: UpdateStakePoolBalanceAccounts,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CleanupRemovedValidatorEntriesAccounts {
    pub stake_pool: Pubkey,
    pub validator_list_storage: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CleanupRemovedValidatorEntriesInstruction {
    pub accounts: CleanupRemovedValidatorEntriesAccounts,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositStakeAccounts {
    pub stake_pool: Pubkey,
    pub validator_list_storage: Pubkey,
    pub stake_pool_deposit_authority: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub deposit_stake_address: Pubkey,
    pub validator_stake_account: Pubkey,
    pub reserve_stake_account: Pubkey,
    pub pool_tokens_to: Pubkey,
    pub manager_fee_account: Pubkey,
    pub referrer_pool_tokens_account: Pubkey,
    pub pool_mint: Pubkey,
    pub clock: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub token_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositStakeInstruction {
    pub accounts: DepositStakeAccounts,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawStakeAccounts {
    pub stake_pool: Pubkey,
    pub validator_list_storage: Pubkey,
    pub stake_pool_withdraw: Pubkey,
    pub stake_to_split: Pubkey,
    pub stake_to_receive: Pubkey,
    pub user_stake_authority: Pubkey,
    pub user_transfer_authority: Pubkey,
    pub user_pool_token_account: Pubkey,
    pub manager_fee_account: Pubkey,
    pub pool_mint: Pubkey,
    pub clock: Pubkey,
    pub token_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawStakeArgs {
    pub amount: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawStakeInstruction {
    pub accounts: WithdrawStakeAccounts,
    pub args: WithdrawStakeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetManagerAccounts {
    pub stake_pool: Pubkey,
    pub manager: Pubkey,
    pub new_manager: Pubkey,
    pub new_fee_receiver: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetManagerInstruction {
    pub accounts: SetManagerAccounts,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetFeeAccounts {
    pub stake_pool: Pubkey,
    pub manager: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetFeeArgs {
    pub fee: Option<FeeType>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetFeeInstruction {
    pub accounts: SetFeeAccounts,
    pub args: SetFeeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetStakerAccounts {
    pub stake_pool: Pubkey,
    pub set_staker_authority: Pubkey,
    pub new_staker: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetStakerInstruction {
    pub accounts: SetStakerAccounts,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositSolAccounts {
    pub stake_pool: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub reserve_stake_account: Pubkey,
    pub lamports_from: Pubkey,
    pub pool_tokens_to: Pubkey,
    pub manager_fee_account: Pubkey,
    pub referrer_pool_tokens_account: Pubkey,
    pub pool_mint: Pubkey,
    pub system_program: Pubkey,
    pub token_program: Pubkey,
    pub deposit_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositSolArgs {
    pub amount: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositSolInstruction {
    pub accounts: DepositSolAccounts,
    pub args: DepositSolArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetFundingAuthorityAccounts {
    pub stake_pool: Pubkey,
    pub manager: Pubkey,
    pub auth: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetFundingAuthorityArgs {
    #[hint(enumeration = "FundingType")]
    pub funding_type: i32,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetFundingAuthorityInstruction {
    pub accounts: SetFundingAuthorityAccounts,
    pub args: SetFundingAuthorityArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawSolAccounts {
    pub stake_pool: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub user_transfer_authority: Pubkey,
    pub pool_tokens_from: Pubkey,
    pub reserve_stake_account: Pubkey,
    pub lamports_to: Pubkey,
    pub manager_fee_account: Pubkey,
    pub pool_mint: Pubkey,
    pub clock: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub stake_program: Pubkey,
    pub token_program: Pubkey,
    pub sol_withdraw_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawSolArgs {
    pub amount: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawSolInstruction {
    pub accounts: WithdrawSolAccounts,
    pub args: WithdrawSolArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CreateTokenMetadataAccounts {
    pub stake_pool: Pubkey,
    pub manager: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub pool_mint: Pubkey,
    pub payer: Pubkey,
    pub token_metadata: Pubkey,
    pub mpl_token_metadata: Pubkey,
    pub system_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CreateTokenMetadataArgs {
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CreateTokenMetadataInstruction {
    pub accounts: CreateTokenMetadataAccounts,
    pub args: CreateTokenMetadataArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateTokenMetadataAccounts {
    pub stake_pool: Pubkey,
    pub manager: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub token_metadata: Pubkey,
    pub mpl_token_metadata: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateTokenMetadataArgs {
    pub name: String,
    pub symbol: String,
    pub uri: String,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpdateTokenMetadataInstruction {
    pub accounts: UpdateTokenMetadataAccounts,
    pub args: UpdateTokenMetadataArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct IncreaseAdditionalValidatorStakeAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list: Pubkey,
    pub reserve_stake: Pubkey,
    pub ephemeral_stake: Pubkey,
    pub transient_stake: Pubkey,
    pub validator_stake: Pubkey,
    pub validator: Pubkey,
    pub clock: Pubkey,
    pub stake_history: Pubkey,
    pub stake_config: Pubkey,
    pub system_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct IncreaseAdditionalValidatorStakeArgs {
    pub lamports: u64,
    pub transient_stake_seed: u64,
    pub ephemeral_stake_seed: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct IncreaseAdditionalValidatorStakeInstruction {
    pub accounts: IncreaseAdditionalValidatorStakeAccounts,
    pub args: IncreaseAdditionalValidatorStakeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseAdditionalValidatorStakeAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list: Pubkey,
    pub reserve_stake: Pubkey,
    pub validator_stake: Pubkey,
    pub ephemeral_stake: Pubkey,
    pub transient_stake: Pubkey,
    pub clock: Pubkey,
    pub stake_history: Pubkey,
    pub system_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseAdditionalValidatorStakeArgs {
    pub lamports: u64,
    pub transient_stake_seed: u64,
    pub ephemeral_stake_seed: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseAdditionalValidatorStakeInstruction {
    pub accounts: DecreaseAdditionalValidatorStakeAccounts,
    pub args: DecreaseAdditionalValidatorStakeArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseValidatorStakeWithReserveAccounts {
    pub stake_pool: Pubkey,
    pub staker: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub validator_list: Pubkey,
    pub reserve_stake: Pubkey,
    pub validator_stake: Pubkey,
    pub transient_stake: Pubkey,
    pub clock: Pubkey,
    pub stake_history: Pubkey,
    pub system_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseValidatorStakeWithReserveArgs {
    pub lamports: u64,
    pub transient_stake_seed: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DecreaseValidatorStakeWithReserveInstruction {
    pub accounts: DecreaseValidatorStakeWithReserveAccounts,
    pub args: DecreaseValidatorStakeWithReserveArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositStakeWithSlippageAccounts {
    pub stake_pool: Pubkey,
    pub validator_list_storage: Pubkey,
    pub stake_pool_deposit_authority: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub deposit_stake_address: Pubkey,
    pub validator_stake_account: Pubkey,
    pub reserve_stake_account: Pubkey,
    pub pool_tokens_to: Pubkey,
    pub manager_fee_account: Pubkey,
    pub referrer_pool_tokens_account: Pubkey,
    pub pool_mint: Pubkey,
    pub clock: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub token_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositStakeWithSlippageArgs {
    pub minimum_pool_tokens_out: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositStakeWithSlippageInstruction {
    pub accounts: DepositStakeWithSlippageAccounts,
    pub args: DepositStakeWithSlippageArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawStakeWithSlippageAccounts {
    pub stake_pool: Pubkey,
    pub validator_list_storage: Pubkey,
    pub stake_pool_withdraw: Pubkey,
    pub stake_to_split: Pubkey,
    pub stake_to_receive: Pubkey,
    pub user_stake_authority: Pubkey,
    pub user_transfer_authority: Pubkey,
    pub user_pool_token_account: Pubkey,
    pub manager_fee_account: Pubkey,
    pub pool_mint: Pubkey,
    pub clock: Pubkey,
    pub token_program: Pubkey,
    pub stake_program: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawStakeWithSlippageArgs {
    pub pool_tokens_in: u64,
    pub minimum_lamports_out: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawStakeWithSlippageInstruction {
    pub accounts: WithdrawStakeWithSlippageAccounts,
    pub args: WithdrawStakeWithSlippageArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositSolWithSlippageAccounts {
    pub stake_pool: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub reserve_stake_account: Pubkey,
    pub lamports_from: Pubkey,
    pub pool_tokens_to: Pubkey,
    pub manager_fee_account: Pubkey,
    pub referrer_pool_tokens_account: Pubkey,
    pub pool_mint: Pubkey,
    pub system_program: Pubkey,
    pub token_program: Pubkey,
    pub deposit_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositSolWithSlippageArgs {
    pub lamports_in: u64,
    pub minimum_pool_tokens_out: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DepositSolWithSlippageInstruction {
    pub accounts: DepositSolWithSlippageAccounts,
    pub args: DepositSolWithSlippageArgs,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawSolWithSlippageAccounts {
    pub stake_pool: Pubkey,
    pub stake_pool_withdraw_authority: Pubkey,
    pub user_transfer_authority: Pubkey,
    pub pool_tokens_from: Pubkey,
    pub reserve_stake_account: Pubkey,
    pub lamports_to: Pubkey,
    pub manager_fee_account: Pubkey,
    pub pool_mint: Pubkey,
    pub clock: Pubkey,
    pub sysvar_stake_history: Pubkey,
    pub stake_program: Pubkey,
    pub token_program: Pubkey,
    pub sol_withdraw_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawSolWithSlippageArgs {
    pub pool_tokens_in: u64,
    pub minimum_lamports_out: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WithdrawSolWithSlippageInstruction {
    pub accounts: WithdrawSolWithSlippageAccounts,
    pub args: WithdrawSolWithSlippageArgs,
}

// ── Conversion helpers ──────────────────────────────────────────────

pub(crate) fn fee_to_proto(fee: spl_stake_pool::state::Fee) -> Fee {
    Fee {
        numerator: fee.numerator,
        denominator: fee.denominator,
    }
}

pub(crate) fn preferred_validator_type_to_proto(
    v: spl_stake_pool::instruction::PreferredValidatorType,
) -> PreferredValidatorType {
    use spl_stake_pool::instruction::PreferredValidatorType as PVT;

    match v {
        PVT::Deposit => PreferredValidatorType::Deposit,
        PVT::Withdraw => PreferredValidatorType::Withdraw,
    }
}

pub(crate) fn funding_type_to_proto(v: spl_stake_pool::instruction::FundingType) -> FundingType {
    use spl_stake_pool::instruction::FundingType as FT;

    match v {
        FT::SolDeposit => FundingType::SolDeposit,
        FT::StakeDeposit => FundingType::StakeDeposit,
        FT::SolWithdraw => FundingType::SolWithdraw,
    }
}

/// StakePool::SetFee carries a `FeeType` (not a `Fee`).
/// `FeeType` is an enum whose variants carry the new `Fee` value.
/// We normalize it into `{ kind, fee }`.
pub(crate) fn fee_type_to_proto(v: spl_stake_pool::state::FeeType) -> FeeType {
    use fee_type::Value;
    use spl_stake_pool::state::FeeType as FT;

    match v {
        FT::SolDeposit(fee) => FeeType {
            kind: FeeKind::SolDeposit as i32,
            value: Some(Value::Fee(fee_to_proto(fee))),
        },
        FT::StakeDeposit(fee) => FeeType {
            kind: FeeKind::StakeDeposit as i32,
            value: Some(Value::Fee(fee_to_proto(fee))),
        },
        FT::SolWithdrawal(fee) => FeeType {
            kind: FeeKind::SolWithdrawal as i32,
            value: Some(Value::Fee(fee_to_proto(fee))),
        },
        FT::StakeWithdrawal(fee) => FeeType {
            kind: FeeKind::StakeWithdrawal as i32,
            value: Some(Value::Fee(fee_to_proto(fee))),
        },
        FT::Epoch(fee) => FeeType {
            kind: FeeKind::Epoch as i32,
            value: Some(Value::Fee(fee_to_proto(fee))),
        },
        FT::SolReferral(bps) => FeeType {
            kind: FeeKind::SolReferral as i32,
            value: Some(Value::ReferralBps(bps as u32)),
        },
        FT::StakeReferral(bps) => FeeType {
            kind: FeeKind::StakeReferral as i32,
            value: Some(Value::ReferralBps(bps as u32)),
        },
    }
}
