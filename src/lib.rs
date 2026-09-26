//! Percolator LP Vault: a permissionless, adminless pooled liquidity provider for Percolator
//! perpetual markets.
//!
//! One program plays two roles. For depositors it is an epoch-settled vault that owns a
//! Percolator LP portfolio. For Percolator it is that portfolio's matcher: takers trade against
//! the vault through `TradeCpi`, and the vault quotes around the market's own price with fixed,
//! immutable risk limits.

pub mod error;
pub mod math;
pub mod matcher;
pub mod percolator;
pub mod processor;
pub mod state;

use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

solana_program::declare_id!("BSync6F8gtJs3Wj4w8L6H3ZtS397w2XoCYGEAJGmeYX");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    match data.first() {
        Some(&matcher::TAG_MATCH) => matcher::process(program_id, accounts, data),
        // Batched fills are not offered: every fill is priced and capped one at a time.
        Some(&matcher::TAG_MATCH_BATCH) => Err(error::VaultError::InvalidInstruction.into()),
        Some(_) => processor::process(program_id, accounts, data),
        None => Err(error::VaultError::InvalidInstruction.into()),
    }
}
