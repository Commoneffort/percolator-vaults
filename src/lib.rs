//! Percolator LP Vault: a permissionless, adminless pooled liquidity provider for Percolator
//! perpetual markets.
//!
//! One program plays two roles. For depositors it is an epoch-settled vault that owns a
//! Percolator LP portfolio. For Percolator it is that portfolio's matcher: takers trade against
//! the vault through `TradeCpi`, and the vault quotes around the market's own price with fixed,
//! immutable risk limits.

#[cfg(not(target_os = "solana"))]
pub mod client;
pub mod error;
pub mod math;
pub mod matcher;
pub mod percolator;
pub mod processor;
pub mod pyth;
pub mod reporter;
pub mod state;

use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey};

solana_program::declare_id!("BSync6F8gtJs3Wj4w8L6H3ZtS397w2XoCYGEAJGmeYX");

/// The router program: the only way to trade against a vault. It queues each trade and fills it
/// at the first Pyth price published a fixed delay after the request landed.
pub const ROUTER_PROGRAM_ID: solana_program::pubkey::Pubkey =
    solana_program::pubkey!("DkK9TSMpVXLq26HeqxTXLysXyRKDYHTKU94SLFDWgjw3");

/// The router's book for a vault (its queue of pending requests), and the byte offset of the
/// pending count in it (pinned against the router's layout in tests).
pub fn router_book(vault: &solana_program::pubkey::Pubkey) -> solana_program::pubkey::Pubkey {
    solana_program::pubkey::Pubkey::find_program_address(&[b"book", vault.as_ref()], &ROUTER_PROGRAM_ID).0
}
pub const ROUTER_BOOK_LEN_OFF: usize = 8 + 32 + 32 + 1;

/// The router's signing authority: the only key that can move a vault's mark or arm a fill.
pub fn router_authority() -> (solana_program::pubkey::Pubkey, u8) {
    solana_program::pubkey::Pubkey::find_program_address(&[b"authority"], &ROUTER_PROGRAM_ID)
}

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
