pub use shipstern_core::Pubkey;
use shipstern_proc_macro::shipstern;

// ── Instruction accounts ───────────────────────────────────────────

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct InitializeBufferAccounts {
    pub buffer: Pubkey,
    pub authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WriteAccounts {
    pub buffer: Pubkey,
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DeployAccounts {
    pub payer: Pubkey,
    pub program_data: Pubkey,
    pub program: Pubkey,
    pub buffer: Pubkey,
    pub rent: Pubkey,
    pub clock: Pubkey,
    pub system_program: Pubkey,
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct UpgradeAccounts {
    pub program_data: Pubkey,
    pub program: Pubkey,
    pub buffer: Pubkey,
    pub spill: Pubkey,
    pub rent: Pubkey,
    pub clock: Pubkey,
    pub authority: Pubkey,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetAuthorityAccounts {
    pub account: Pubkey,
    pub current_authority: Pubkey,
    pub new_authority: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct CloseAccounts {
    pub close_target: Pubkey,
    pub recipient: Pubkey,
    pub authority: Option<Pubkey>,
    pub program: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ExtendProgramAccounts {
    pub program_data: Pubkey,
    pub program: Pubkey,
    pub system_program: Option<Pubkey>,
    pub payer: Option<Pubkey>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct SetAuthorityCheckedAccounts {
    pub account: Pubkey,
    pub current_authority: Pubkey,
    pub new_authority: Pubkey,
}

// ── Instruction args ───────────────────────────────────────────────

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct WriteArgs {
    pub offset: u32,
    pub bytes: Vec<u8>,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct DeployArgs {
    pub max_data_len: u64,
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct ExtendProgramArgs {
    pub additional_bytes: u32,
}

// ── Instruction wrappers ───────────────────────────────────────────

pub mod instruction {
    use super::shipstern;

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct InitializeBuffer {
        pub accounts: Option<super::InitializeBufferAccounts>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Write {
        pub accounts: Option<super::WriteAccounts>,
        pub args: Option<super::WriteArgs>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Deploy {
        pub accounts: Option<super::DeployAccounts>,
        pub args: Option<super::DeployArgs>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Upgrade {
        pub accounts: Option<super::UpgradeAccounts>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct SetAuthority {
        pub accounts: Option<super::SetAuthorityAccounts>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct Close {
        pub accounts: Option<super::CloseAccounts>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct ExtendProgram {
        pub accounts: Option<super::ExtendProgramAccounts>,
        pub args: Option<super::ExtendProgramArgs>,
    }

    #[shipstern]
    #[derive(Clone, PartialEq)]
    pub struct SetAuthorityChecked {
        pub accounts: Option<super::SetAuthorityCheckedAccounts>,
    }

    #[shipstern(oneof)]
    #[derive(Clone, PartialEq)]
    pub enum Instruction {
        InitializeBuffer(InitializeBuffer),
        Write(Write),
        Deploy(Deploy),
        Upgrade(Upgrade),
        SetAuthority(SetAuthority),
        Close(Close),
        ExtendProgram(ExtendProgram),
        SetAuthorityChecked(SetAuthorityChecked),
    }
}

#[shipstern]
#[derive(Clone, PartialEq)]
pub struct BpfLoaderProgram {
    #[hint(oneof = "instruction::Instruction", tags = "1, 2, 3, 4, 5, 6, 7, 8")]
    pub instruction: Option<instruction::Instruction>,
}
