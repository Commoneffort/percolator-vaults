//! Account layouts. Every struct is `repr(C, packed)` plain old data, copied in and out of the
//! account buffer, so reads never depend on buffer alignment.

use bytemuck::{Pod, Zeroable};
use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

use crate::error::VaultError;

pub const VAULT_MAGIC: u64 = u64::from_le_bytes(*b"PLVAULT1");
pub const TICKET_MAGIC: u64 = u64::from_le_bytes(*b"PLTICKT1");
pub const EPOCH_MAGIC: u64 = u64::from_le_bytes(*b"PLEPOCH1");

/// Operate mode only: created, waiting for `ListAsset` to list the vault's own asset.
pub const STATUS_PENDING_LISTING: u8 = 0;
pub const STATUS_ACTIVE: u8 = 1;
pub const STATUS_TERMINAL: u8 = 2;

/// The vault provides liquidity on an asset someone else listed.
pub const MODE_ATTACH: u8 = 0;
/// The vault lists its own asset, holds every authority over it, and harvests its trading fees.
pub const MODE_OPERATE: u8 = 1;

pub const KIND_LEGACY: u8 = 0;
pub const KIND_CANONICAL: u8 = 1;

/// The first 64 bytes of the vault account are Percolator's matcher return slot: the vault
/// account is also the matcher context Percolator passes to the vault's matcher entrypoint.
pub const MATCHER_RETURN_LEN: usize = 64;
pub const VAULT_STATE_OFF: usize = MATCHER_RETURN_LEN;

#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C, packed)]
pub struct VaultState {
    pub magic: u64,
    pub version: u8,
    pub status: u8,
    pub vault_bump: u8,
    pub portfolio_bump: u8,
    pub share_mint_bump: u8,
    pub buffer_bump: u8,
    pub escrow_bump: u8,
    pub share_decimals: u8,

    pub market: Pubkey,
    pub creator: Pubkey,
    pub seed: u64,
    pub collateral_mint: Pubkey,
    pub share_mint: Pubkey,
    pub buffer: Pubkey,
    pub share_escrow: Pubkey,
    pub lp_portfolio: Pubkey,
    pub matcher_delegate: Pubkey,
    pub portfolio_id: u64,

    // Immutable parameters, fixed at creation. There is no instruction that changes them.
    pub asset_index: u16,
    pub spread_bps: u16,
    pub unwind_spread_bps: u16,
    pub trade_fee_cap_bps: u16,
    pub backing_fee_cap_bps: u16,
    /// Position cap as a multiple of the vault's NAV (basis points of NAV in notional); 0 = off.
    pub position_nav_bps: u16,
    /// Per-fill cap as a multiple of the vault's NAV (basis points of NAV in notional); 0 = off.
    pub fill_nav_bps: u16,
    pub _pad0: [u8; 2],
    pub max_fill_abs: u128,
    pub max_inventory_abs: u128,
    pub epoch_len_slots: u64,
    pub matcher_ttl_slots: u64,

    // Operate mode: listing and oracle parameters, and the insurance kept for traders.
    pub mode: u8,
    pub oracle_leg_count: u8,
    pub oracle_leg_flags: u8,
    pub oracle_invert: u8,
    pub oracle_unit_scale: u32,
    pub oracle_conf_filter_bps: u16,
    /// KIND_CANONICAL vaults live at the one address for (market, feed); KIND_LEGACY at
    /// (market, creator, seed).
    pub vault_kind: u8,
    pub _pad1: [u8; 5],
    pub oracle_max_staleness_secs: u64,
    pub oracle_soft_stale_slots: u64,
    pub oracle_ewma_halflife_slots: u64,
    pub oracle_mark_min_fee: u64,
    pub oracle_feeds: [[u8; 32]; 3],
    /// Insurance (long + short budget of the vault's asset) that fee harvesting never touches.
    pub insurance_floor: u64,
    /// Most the lister may pay Percolator's permissionless listing fee.
    pub listing_fee_max: u64,
    /// Generation (`market_id`) of the vault's asset, recorded when it was attached or listed.
    pub asset_market_id: u64,

    // Dynamic state.
    /// The vault's position in `asset_index` as seen by its matcher (positive = long).
    /// Re-anchored to zero at every roll, where the portfolio is proven flat.
    pub inventory: i128,
    pub epoch: u64,
    pub epoch_start_slot: u64,
    /// Collateral waiting in the buffer for the current epoch's deposit requests.
    pub pending_deposit_assets: u64,
    /// Shares waiting in escrow for the current epoch's withdrawal requests.
    pub pending_withdraw_shares: u64,
    /// Collateral in the buffer owed to past epochs' withdrawals and refunds, not yet claimed.
    pub reserved_assets: u64,
    /// Net asset value after the last roll (conservative).
    pub last_nav: u64,
    pub created_slot: u64,
    pub total_fills: u64,
    pub total_fees_harvested: u64,
    /// Canonical vaults: the opener's share of harvested fees, set aside in the buffer (and
    /// counted in `reserved_assets`) until the opener claims it.
    pub opener_fees_owed: u64,
    pub opener_fees_total: u64,
    /// Slot the vault's current asset was listed (0 for vaults listed before this field).
    pub listed_slot: u64,
    pub _reserved: [u8; 32],
}

pub const VAULT_STATE_LEN: usize = core::mem::size_of::<VaultState>();
pub const VAULT_ACCOUNT_LEN: usize = VAULT_STATE_OFF + VAULT_STATE_LEN;

#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C, packed)]
pub struct Ticket {
    pub magic: u64,
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub epoch: u64,
    pub deposit_assets: u64,
    pub withdraw_shares: u64,
    pub bump: u8,
    pub _pad: [u8; 7],
}
pub const TICKET_LEN: usize = core::mem::size_of::<Ticket>();

#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C, packed)]
pub struct EpochRecord {
    pub magic: u64,
    pub vault: Pubkey,
    pub epoch: u64,
    pub deposit_assets: u64,
    pub shares_minted: u64,
    pub withdraw_shares: u64,
    pub assets_out: u64,
    pub nav_low: u64,
    pub nav_high: u64,
    pub supply_before: u64,
    pub rolled_slot: u64,
    /// 1 when the epoch's deposits could not be priced (vault wiped out) and are refunded.
    pub deposits_refunded: u8,
    pub bump: u8,
    pub _pad: [u8; 6],
}
pub const EPOCH_RECORD_LEN: usize = core::mem::size_of::<EpochRecord>();

fn load<T: Pod>(ai: &AccountInfo, off: usize) -> Result<T, ProgramError> {
    let d = ai.try_borrow_data()?;
    let len = core::mem::size_of::<T>();
    let b = d.get(off..off + len).ok_or(VaultError::BadAccount)?;
    Ok(bytemuck::pod_read_unaligned(b))
}

fn store<T: Pod>(ai: &AccountInfo, off: usize, v: &T) -> Result<(), ProgramError> {
    let mut d = ai.try_borrow_mut_data()?;
    let len = core::mem::size_of::<T>();
    d.get_mut(off..off + len)
        .ok_or(VaultError::BadAccount)?
        .copy_from_slice(bytemuck::bytes_of(v));
    Ok(())
}

/// Loads the vault after checking owner, size and magic.
pub fn load_vault(ai: &AccountInfo, program_id: &Pubkey) -> Result<VaultState, ProgramError> {
    if ai.owner != program_id || ai.data_len() != VAULT_ACCOUNT_LEN {
        return Err(VaultError::BadAccount.into());
    }
    let v: VaultState = load(ai, VAULT_STATE_OFF)?;
    if v.magic != VAULT_MAGIC {
        return Err(VaultError::BadAccount.into());
    }
    Ok(v)
}

pub fn store_vault(ai: &AccountInfo, v: &VaultState) -> Result<(), ProgramError> {
    store(ai, VAULT_STATE_OFF, v)
}

pub fn load_ticket(ai: &AccountInfo, program_id: &Pubkey) -> Result<Ticket, ProgramError> {
    if ai.owner != program_id || ai.data_len() != TICKET_LEN {
        return Err(VaultError::BadAccount.into());
    }
    let t: Ticket = load(ai, 0)?;
    if t.magic != TICKET_MAGIC {
        return Err(VaultError::BadAccount.into());
    }
    Ok(t)
}

pub fn store_ticket(ai: &AccountInfo, t: &Ticket) -> Result<(), ProgramError> {
    store(ai, 0, t)
}

pub fn load_epoch(ai: &AccountInfo, program_id: &Pubkey) -> Result<EpochRecord, ProgramError> {
    if ai.owner != program_id || ai.data_len() != EPOCH_RECORD_LEN {
        return Err(VaultError::BadAccount.into());
    }
    let r: EpochRecord = load(ai, 0)?;
    if r.magic != EPOCH_MAGIC {
        return Err(VaultError::BadAccount.into());
    }
    Ok(r)
}

pub fn store_epoch(ai: &AccountInfo, r: &EpochRecord) -> Result<(), ProgramError> {
    store(ai, 0, r)
}

// ---- PDA seeds ----
pub const SEED_VAULT: &[u8] = b"vault";
pub const SEED_CANONICAL: &[u8] = b"canon";
pub const SEED_SHARES: &[u8] = b"shares";
pub const SEED_BUFFER: &[u8] = b"buffer";
pub const SEED_ESCROW: &[u8] = b"escrow";
pub const SEED_PORTFOLIO: &[u8] = b"portfolio";
pub const SEED_TICKET: &[u8] = b"ticket";
pub const SEED_EPOCH: &[u8] = b"epoch";
/// The market-level authority (`marketauth`) of markets the vault program governs. The program
/// uses it for one thing only: retiring an idle, empty market so its slot can be reused.
pub const SEED_GOVERNOR: &[u8] = b"governor";

pub fn governor_address(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_GOVERNOR, market.as_ref()], program_id)
}

pub fn vault_address(
    program_id: &Pubkey,
    market: &Pubkey,
    creator: &Pubkey,
    seed: u64,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_VAULT, market.as_ref(), creator.as_ref(), &seed.to_le_bytes()],
        program_id,
    )
}

/// The one operate-mode vault a market can have for a price feed.
pub fn canonical_vault_address(program_id: &Pubkey, market: &Pubkey, feed: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_CANONICAL, market.as_ref(), feed, &0u64.to_le_bytes()],
        program_id,
    )
}

pub fn child_address(program_id: &Pubkey, tag: &[u8], vault: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[tag, vault.as_ref()], program_id)
}

pub fn ticket_address(program_id: &Pubkey, vault: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_TICKET, vault.as_ref(), owner.as_ref()], program_id)
}

pub fn epoch_address(program_id: &Pubkey, vault: &Pubkey, epoch: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[SEED_EPOCH, vault.as_ref(), &epoch.to_le_bytes()],
        program_id,
    )
}
