//! The vault's matcher: the code Percolator calls (by CPI, during a taker's `TradeCpi`) to price
//! a fill against the vault's LP portfolio.
//!
//! Trust: only Percolator can sign for the matcher delegate PDA of the vault's own portfolio, so
//! a call signed by that PDA comes from Percolator, for this vault, and its request fields
//! (request id, asset, oracle price, size) are authentic. Percolator then validates the echo and
//! executes exactly the size and price written here, or the whole transaction fails.

use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::{
    error::VaultError,
    math,
    state::{self, STATUS_ACTIVE},
};

pub const TAG_MATCH: u8 = 0;
pub const TAG_MATCH_BATCH: u8 = 3;
const REQUEST_LEN: usize = 67;
const ABI_VERSION: u32 = 3;
const FLAG_VALID: u32 = 1;
const FLAG_PARTIAL_OK: u32 = 2;
const FLAG_BACKING_FEE_CAP_SHIFT: u32 = 8;

pub struct Request {
    pub req_id: u64,
    pub asset_index: u16,
    pub lp_account_id: u64,
    pub oracle_price_e6: u64,
    pub req_size: i128,
}

pub fn decode(data: &[u8]) -> Result<Request, VaultError> {
    if data.len() != REQUEST_LEN || data[0] != TAG_MATCH || data[43..].iter().any(|b| *b != 0) {
        return Err(VaultError::InvalidInstruction);
    }
    Ok(Request {
        req_id: u64::from_le_bytes(data[1..9].try_into().unwrap()),
        asset_index: u16::from_le_bytes(data[9..11].try_into().unwrap()),
        lp_account_id: u64::from_le_bytes(data[11..19].try_into().unwrap()),
        oracle_price_e6: u64::from_le_bytes(data[19..27].try_into().unwrap()),
        req_size: i128::from_le_bytes(data[27..43].try_into().unwrap()),
    })
}

/// True when the vault only accepts fills that shrink its position: the epoch is over and
/// someone is waiting to deposit or withdraw, so the vault works toward flat to settle them.
pub fn reduce_only(v: &state::VaultState, now_slot: u64) -> bool {
    let epoch_over = now_slot >= v.epoch_start_slot.saturating_add(v.epoch_len_slots);
    let has_requests = v.pending_deposit_assets != 0 || v.pending_withdraw_shares != 0;
    epoch_over && has_requests
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let req = decode(data)?;
    let delegate = accounts.first().ok_or(VaultError::BadAccount)?;
    let ctx = accounts.get(1).ok_or(VaultError::BadAccount)?;
    if !ctx.is_writable {
        return Err(VaultError::NotWritable.into());
    }
    let mut v = state::load_vault(ctx, program_id)?;
    if !delegate.is_signer || *delegate.key != v.matcher_delegate {
        return Err(VaultError::UnauthorizedMatcherCaller.into());
    }
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    if req.asset_index != v.asset_index {
        return Err(VaultError::UnknownAsset.into());
    }
    if req.req_size == 0 || req.req_size == i128::MIN || req.oracle_price_e6 == 0 {
        return Err(VaultError::InvalidInstruction.into());
    }

    let now = Clock::get()?.slot;
    // Only the fill the router armed for this slot and size: trades cannot bypass the router's
    // sequencing, which is what makes them impossible to front-run. The arm is used up here.
    if v.armed_slot != now || v.armed_size != req.req_size {
        return Err(VaultError::NotArmed.into());
    }
    v.armed_size = 0;
    v.armed_slot = 0;
    let ro = reduce_only(&v, now);
    let taker_buys = req.req_size > 0;
    // The vault takes the other side: a taker buy moves the vault short.
    let lp_sign: i8 = if taker_buys { -1 } else { 1 };
    // Caps are the tighter of the fixed limits and, when configured, multiples of the vault's
    // NAV at the price of this request. An empty vault therefore quotes nothing.
    let (mut max_fill, mut max_inventory) = (v.max_fill_abs, v.max_inventory_abs);
    if v.position_nav_bps != 0 {
        let nav = v.last_nav;
        max_inventory = max_inventory.min(math::nav_cap(nav, v.position_nav_bps, req.oracle_price_e6));
        max_fill = max_fill.min(math::nav_cap(nav, v.fill_nav_bps, req.oracle_price_e6));
    }
    let fill = math::allowed_fill(
        req.req_size.unsigned_abs(),
        max_fill,
        v.inventory,
        lp_sign,
        max_inventory,
        ro,
    );

    let (exec_price, exec_size) = if fill == 0 {
        // A valid zero fill: Percolator requires the oracle price echoed as the price.
        (req.oracle_price_e6, 0i128)
    } else {
        let spread = if ro { v.unwind_spread_bps } else { v.spread_bps };
        let price = math::quote_price(req.oracle_price_e6, taker_buys, spread)?;
        let size = i128::try_from(fill).map_err(|_| VaultError::Overflow)?;
        (price, if taker_buys { size } else { -size })
    };

    // Update the tracked inventory. Checked: an overflow aborts the whole trade.
    v.inventory = v
        .inventory
        .checked_sub(exec_size)
        .ok_or(VaultError::Overflow)?;
    if exec_size != 0 {
        v.total_fills = v.total_fills.saturating_add(1);
    }
    state::store_vault(ctx, &v)?;

    let flags = FLAG_VALID
        | FLAG_PARTIAL_OK
        | ((v.backing_fee_cap_bps as u32) << FLAG_BACKING_FEE_CAP_SHIFT);
    let mut ret = [0u8; state::MATCHER_RETURN_LEN];
    ret[0..4].copy_from_slice(&ABI_VERSION.to_le_bytes());
    ret[4..8].copy_from_slice(&flags.to_le_bytes());
    ret[8..16].copy_from_slice(&exec_price.to_le_bytes());
    ret[16..32].copy_from_slice(&exec_size.to_le_bytes());
    ret[32..40].copy_from_slice(&req.req_id.to_le_bytes());
    ret[40..48].copy_from_slice(&req.lp_account_id.to_le_bytes());
    ret[48..56].copy_from_slice(&req.oracle_price_e6.to_le_bytes());
    ret[56..64].copy_from_slice(&(req.asset_index as u64).to_le_bytes());
    ctx.try_borrow_mut_data()?[..state::MATCHER_RETURN_LEN].copy_from_slice(&ret);
    Ok(())
}
