use solana_program::program_error::ProgramError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum VaultError {
    InvalidInstruction = 0x5600,
    InvalidParams,
    BadAccount,
    BadPda,
    BadPercolatorAccount,
    NotSigner,
    NotWritable,
    AlreadyInitialized,
    NotActive,
    NotTerminal,
    ClaimFirst,
    NothingToClaim,
    EpochNotOver,
    EpochNotRolled,
    NotFlat,
    Overflow,
    ZeroAmount,
    UnknownAsset,
    UnauthorizedMatcherCaller,
    StillLive,
    WrongMode,
    NothingToHarvest,
}

impl From<VaultError> for ProgramError {
    fn from(e: VaultError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
