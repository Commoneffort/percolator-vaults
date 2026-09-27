//! Router account layouts (packed plain old data) and addresses.

use bytemuck::{Pod, Zeroable};
use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

use crate::RouterError;

pub const TRADER_MAGIC: u64 = u64::from_le_bytes(*b"PRTRADR1");
pub const BOOK_MAGIC: u64 = u64::from_le_bytes(*b"PRBOOK01");
pub const REQUEST_MAGIC: u64 = u64::from_le_bytes(*b"PRREQST1");
pub const MAX_PENDING: usize = 32;

pub const SEED_AUTHORITY: &[u8] = b"authority";
pub const SEED_TRADER: &[u8] = b"trader";
pub const SEED_PORTFOLIO: &[u8] = b"portfolio";
pub const SEED_COLLATERAL: &[u8] = b"collateral";
pub const SEED_BOOK: &[u8] = b"book";
pub const SEED_REQUEST: &[u8] = b"request";

/// A trader's router account. Its address owns the trader's Percolator portfolio and collateral
/// token account; only the router can sign for it.
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C, packed)]
pub struct TraderState {
    pub magic: u64,
    pub market: Pubkey,
    pub wallet: Pubkey,
    pub portfolio: Pubkey,
    pub collateral: Pubkey,
    pub bump: u8,
    pub has_pending: u8,
    pub _pad: [u8; 6],
    pub pending_vault: Pubkey,
    pub pending_id: u64,
}
pub const TRADER_LEN: usize = core::mem::size_of::<TraderState>();

/// A vault's queue of pending requests and the Pyth update its mark currently holds.
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C, packed)]
pub struct Book {
    pub magic: u64,
    pub vault: Pubkey,
    pub market: Pubkey,
    pub bump: u8,
    pub len: u8,
    pub _pad: [u8; 6],
    /// Publish time, previous publish time and price (e6) of the Pyth update the mark holds.
    pub mark_publish_time: i64,
    pub mark_prev_publish_time: i64,
    pub mark_price: u64,
    pub next_id: u64,
    /// Bonds of expired requests, kept in this account (nothing can withdraw them).
    pub forfeited_bonds: u64,
    pub pending_id: [u64; MAX_PENDING],
    pub pending_target: [i64; MAX_PENDING],
}
pub const BOOK_LEN: usize = core::mem::size_of::<Book>();

impl Book {
    pub fn earliest_target(&self) -> Option<i64> {
        let targets = self.pending_target;
        targets[..self.len as usize].iter().copied().min()
    }

    pub fn remove(&mut self, id: u64) -> Result<(), RouterError> {
        let n = self.len as usize;
        let (mut ids, mut targets) = (self.pending_id, self.pending_target);
        let i = ids[..n].iter().position(|x| *x == id).ok_or(RouterError::BadAccount)?;
        ids[i] = ids[n - 1];
        targets[i] = targets[n - 1];
        ids[n - 1] = 0;
        targets[n - 1] = 0;
        self.pending_id = ids;
        self.pending_target = targets;
        self.len -= 1;
        Ok(())
    }
}

/// One queued trade. Its target time is fixed when it lands; it fills at the first Pyth price
/// published at or after it, or expires.
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C, packed)]
pub struct Request {
    pub magic: u64,
    pub vault: Pubkey,
    pub wallet: Pubkey,
    pub id: u64,
    pub size: i128,
    pub target_time: i64,
    pub created_slot: u64,
    pub bump: u8,
    pub _pad: [u8; 7],
}
pub const REQUEST_LEN: usize = core::mem::size_of::<Request>();

pub fn load<T: Pod>(ai: &AccountInfo, program_id: &Pubkey, magic: u64) -> Result<T, ProgramError> {
    if ai.owner != program_id || ai.data_len() != core::mem::size_of::<T>() {
        return Err(RouterError::BadAccount.into());
    }
    let d = ai.try_borrow_data()?;
    if d[..8] != magic.to_le_bytes() {
        return Err(RouterError::BadAccount.into());
    }
    Ok(bytemuck::pod_read_unaligned(&d))
}

pub fn store<T: Pod>(ai: &AccountInfo, v: &T) -> Result<(), ProgramError> {
    ai.try_borrow_mut_data()?.copy_from_slice(bytemuck::bytes_of(v));
    Ok(())
}

pub fn authority_address(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_AUTHORITY], program_id)
}
pub fn trader_address(program_id: &Pubkey, market: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_TRADER, market.as_ref(), wallet.as_ref()], program_id)
}
pub fn portfolio_address(program_id: &Pubkey, market: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_PORTFOLIO, market.as_ref(), wallet.as_ref()], program_id)
}
pub fn collateral_address(program_id: &Pubkey, market: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_COLLATERAL, market.as_ref(), wallet.as_ref()], program_id)
}
pub fn book_address(program_id: &Pubkey, vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_BOOK, vault.as_ref()], program_id)
}
pub fn request_address(program_id: &Pubkey, vault: &Pubkey, id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_REQUEST, vault.as_ref(), &id.to_le_bytes()], program_id)
}
