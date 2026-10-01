//! Everything the vault knows about Percolator: its program ID, the few account-layout offsets
//! it reads, and encoders for the handful of instructions it calls.
//!
//! The offsets are hardcoded to keep the on-chain program free of the engine crate. The test
//! `layout_matches_engine` checks every one of them against the pinned engine and wrapper crates,
//! so a layout change in Percolator fails the build of the tests instead of silently misreading.

use solana_program::{
    account_info::AccountInfo,
    instruction::{AccountMeta, Instruction},
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::error::VaultError;

#[cfg(feature = "devnet")]
pub const PERCOLATOR_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P");
#[cfg(not(feature = "devnet"))]
pub const PERCOLATOR_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("Perco1ator111111111111111111111111111111111");

// ---- account header (both market and portfolio accounts) ----
pub const MAGIC: u64 = 0x5045_5243_5631_3600; // "PERCV16\0"
pub const VERSION: u16 = 16;
pub const KIND_MARKET: u8 = 1;
pub const KIND_PORTFOLIO: u8 = 2;
pub const HEADER_LEN: usize = 16;

// ---- market account ----
/// `WrapperConfigV16.collateral_mint` (primary mint), right after `marketauth`.
pub const MARKET_COLLATERAL_MINT_OFF: usize = HEADER_LEN + 32;

/// Per-asset slots follow the wrapper config and the market-group header.
pub const MARKET_SLOTS_OFF: usize = 464 + 726;
pub const MARKET_ASSET_SLOT_LEN: usize = 1813;
const SLOT_ENGINE: usize = 512;
const SLOT_MARKET_ID: usize = SLOT_ENGINE;
const SLOT_INS_BUDGET_LONG: usize = SLOT_ENGINE + 515;
const SLOT_INS_BUDGET_SHORT: usize = SLOT_ENGINE + 531;
const SLOT_INS_SPENT_LONG: usize = SLOT_ENGINE + 547;
const SLOT_INS_SPENT_SHORT: usize = SLOT_ENGINE + 563;

pub fn market_account_len_for_slots(slots: usize) -> usize {
    MARKET_SLOTS_OFF + slots * MARKET_ASSET_SLOT_LEN
}

/// Generation and remaining insurance budget (long + short) of one asset.
pub fn asset_insurance(ai: &AccountInfo, asset_index: u16) -> Result<(u64, u128), ProgramError> {
    expect_market(ai)?;
    let d = ai.try_borrow_data()?;
    let base = MARKET_SLOTS_OFF + asset_index as usize * MARKET_ASSET_SLOT_LEN;
    let market_id = rd_u64(&d, base + SLOT_MARKET_ID)?;
    let long = rd_u128(&d, base + SLOT_INS_BUDGET_LONG)?
        .saturating_sub(rd_u128(&d, base + SLOT_INS_SPENT_LONG)?);
    let short = rd_u128(&d, base + SLOT_INS_BUDGET_SHORT)?
        .saturating_sub(rd_u128(&d, base + SLOT_INS_SPENT_SHORT)?);
    Ok((market_id, long.saturating_add(short)))
}

// ---- portfolio account ----
pub const PORTFOLIO_ACCOUNT_LEN: usize = 9563;
const ENGINE: usize = HEADER_LEN;
pub const PORTFOLIO_MARKET_OFF: usize = ENGINE; // provenance.market_group_id
pub const PORTFOLIO_SELF_OFF: usize = ENGINE + 32; // provenance.portfolio_account_id
pub const PORTFOLIO_OWNER_OFF: usize = ENGINE + 100; // engine owner (mirrors provenance)
pub const PORTFOLIO_CAPITAL_OFF: usize = ENGINE + 132;
pub const PORTFOLIO_PNL_OFF: usize = ENGINE + 148;
pub const PORTFOLIO_FEE_CREDITS_OFF: usize = ENGINE + 292;
pub const PORTFOLIO_ACTIVE_BITMAP_OFF: usize = ENGINE + 332;
pub const PORTFOLIO_MATCHER_CONTROL_OFF: usize = 9435 + 96;
pub const PORTFOLIO_ID_OFF: usize = 9435 + 104;
pub const PORTFOLIO_SEQUENCE_OFF: usize = PORTFOLIO_ID_OFF + 8;

const POSITION_EPOCH_SHIFT: u32 = 1;
const POSITION_EPOCH_MASK: u64 = ((1u64 << 49) - 1) << POSITION_EPOCH_SHIFT;

// ---- instruction tags ----
const TAG_INIT_PORTFOLIO: u8 = 1;
const TAG_DEPOSIT: u8 = 3;
const TAG_WITHDRAW: u8 = 4;
const TAG_CONVERT_RELEASED_PNL: u8 = 28;
const TAG_CLOSE_RESOLVED: u8 = 30;
const TAG_SYNC_MAINTENANCE_FEE: u8 = 48;
const TAG_SET_MATCHER_CONFIG: u8 = 68;
const TAG_UPDATE_ASSET_LIFECYCLE: u8 = 40;
const TAG_CONFIGURE_HYBRID_ORACLE: u8 = 34;
const TAG_WITHDRAW_INSURANCE_ASSET: u8 = 57;
pub const ASSET_ACTION_ACTIVATE: u8 = 0;
const ASSET_ACTION_RETIRE: u8 = 2;
const TAG_UPDATE_AUTHORITY: u8 = 32;
const TAG_CONFIGURE_AUTH_MARK: u8 = 62;
const TAG_PUSH_AUTH_MARK: u8 = 63;
const TAG_REBALANCE_REDUCE: u8 = 44;
const TAG_TRADE_CPI: u8 = 10;

fn rd_u64(d: &[u8], off: usize) -> Result<u64, ProgramError> {
    let b = d.get(off..off + 8).ok_or(VaultError::BadPercolatorAccount)?;
    Ok(u64::from_le_bytes(b.try_into().unwrap()))
}
fn rd_u128(d: &[u8], off: usize) -> Result<u128, ProgramError> {
    let b = d.get(off..off + 16).ok_or(VaultError::BadPercolatorAccount)?;
    Ok(u128::from_le_bytes(b.try_into().unwrap()))
}
fn rd_i128(d: &[u8], off: usize) -> Result<i128, ProgramError> {
    Ok(rd_u128(d, off)? as i128)
}
fn rd_key(d: &[u8], off: usize) -> Result<Pubkey, ProgramError> {
    let b = d.get(off..off + 32).ok_or(VaultError::BadPercolatorAccount)?;
    Ok(Pubkey::new_from_array(b.try_into().unwrap()))
}

fn check_header(d: &[u8], kind: u8) -> Result<(), ProgramError> {
    if d.len() < HEADER_LEN
        || rd_u64(d, 0)? != MAGIC
        || u16::from_le_bytes([d[8], d[9]]) != VERSION
        || d[10] != kind
    {
        return Err(VaultError::BadPercolatorAccount.into());
    }
    Ok(())
}

pub fn expect_market(ai: &AccountInfo) -> Result<(), ProgramError> {
    if ai.owner != &PERCOLATOR_PROGRAM_ID {
        return Err(VaultError::BadPercolatorAccount.into());
    }
    check_header(&ai.try_borrow_data()?, KIND_MARKET)
}

// ---- market config and asset price (offsets pinned to Percolator's types in tests/vault.rs) ----
pub const MARKET_CONFIG_OFF: usize = 464 + 32;
pub const CONFIG_INITIAL_MARGIN_BPS: usize = 62;
pub const CONFIG_MIN_NONZERO_IM_REQ: usize = 22;
pub const WRAPPER_TRADE_FEE_BASE_BPS: usize = HEADER_LEN + 128;
pub const ASSET_EFFECTIVE_PRICE: usize = 25;
pub const ASSET_SLOT_LAST: usize = 41;
pub const ASSET_A_LONG: usize = 49;
pub const ASSET_A_SHORT: usize = 65;
/// Percolator's deleveraging coefficient of a side nothing has been force-reduced against.
pub const ADL_ONE: u128 = 1_000_000_000_000_000;
pub const PORTFOLIO_LEGS_OFF: usize = 356;
pub const PORTFOLIO_LEG_LEN: usize = 152;
pub const PORTFOLIO_LEG_COUNT: usize = 16;
pub const LEG_ACTIVE: usize = 0;
pub const LEG_ASSET_INDEX: usize = 1;
pub const LEG_SIDE: usize = 13;
pub const LEG_BASIS_POS_Q: usize = 14;

/// Market-wide margin parameters: initial margin in bps, the minimum nonzero initial margin
/// requirement (quote atoms) and the base trade fee in bps.
pub fn margin_params(d: &[u8]) -> Result<(u64, u128, u64), ProgramError> {
    Ok((
        rd_u64(d, MARKET_CONFIG_OFF + CONFIG_INITIAL_MARGIN_BPS)?,
        rd_u128(d, MARKET_CONFIG_OFF + CONFIG_MIN_NONZERO_IM_REQ)?,
        rd_u64(d, WRAPPER_TRADE_FEE_BASE_BPS)?,
    ))
}

/// An asset's generation (`market_id`), effective price (e6) and last accrual slot.
pub fn asset_price(d: &[u8], asset_index: u16) -> Result<(u64, u64, u64), ProgramError> {
    let e = MARKET_SLOTS_OFF + asset_index as usize * MARKET_ASSET_SLOT_LEN + SLOT_ENGINE;
    Ok((rd_u64(d, e)?, rd_u64(d, e + ASSET_EFFECTIVE_PRICE)?, rd_u64(d, e + ASSET_SLOT_LAST)?))
}

/// True while an asset is close-only: a position on it was reduced unilaterally (the vault's
/// `Unwind`, a liquidation), which deleverages the other side, and Percolator then refuses every
/// risk-increasing trade on the asset until all positions on it are closed and its sides reset.
pub fn asset_close_only(d: &[u8], asset_index: u16) -> Result<bool, ProgramError> {
    let e = MARKET_SLOTS_OFF + asset_index as usize * MARKET_ASSET_SLOT_LEN + SLOT_ENGINE;
    Ok(rd_u128(d, e + ASSET_A_LONG)? != ADL_ONE || rd_u128(d, e + ASSET_A_SHORT)? != ADL_ONE)
}

/// A portfolio's capital, PnL and active legs as (asset, signed position: positive is long).
pub fn portfolio_exposure(d: &[u8]) -> Result<(u128, i128, Vec<(u16, i128)>), ProgramError> {
    let mut legs = Vec::new();
    for i in 0..PORTFOLIO_LEG_COUNT {
        let l = PORTFOLIO_LEGS_OFF + i * PORTFOLIO_LEG_LEN;
        let active = *d.get(l + LEG_ACTIVE).ok_or(VaultError::BadPercolatorAccount)?;
        if active != 1 {
            continue;
        }
        let asset = u32::from_le_bytes(d[l + LEG_ASSET_INDEX..l + LEG_ASSET_INDEX + 4].try_into().unwrap());
        let q = rd_i128(d, l + LEG_BASIS_POS_Q)?;
        // The side is stored separately; the size's own sign is not relied on.
        let abs = i128::try_from(q.unsigned_abs()).map_err(|_| VaultError::BadPercolatorAccount)?;
        let signed = if d[l + LEG_SIDE] == 0 { abs } else { -abs };
        legs.push((u16::try_from(asset).map_err(|_| VaultError::BadPercolatorAccount)?, signed));
    }
    Ok((rd_u128(d, PORTFOLIO_CAPITAL_OFF)?, rd_i128(d, PORTFOLIO_PNL_OFF)?, legs))
}

pub fn market_collateral_mint(ai: &AccountInfo) -> Result<Pubkey, ProgramError> {
    expect_market(ai)?;
    rd_key(&ai.try_borrow_data()?, MARKET_COLLATERAL_MINT_OFF)
}

/// The fields of the vault's LP portfolio the vault acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortfolioView {
    pub capital: u128,
    pub pnl: i128,
    pub fee_credits: i128,
    pub flat: bool,
    pub portfolio_id: u64,
    pub sequence: u64,
    pub position_epoch: u64,
}

/// Reads the portfolio after checking it is a Percolator portfolio of `market` owned by `owner`
/// and stored at its own address.
pub fn read_portfolio(
    ai: &AccountInfo,
    market: &Pubkey,
    owner: &Pubkey,
) -> Result<PortfolioView, ProgramError> {
    if ai.owner != &PERCOLATOR_PROGRAM_ID {
        return Err(VaultError::BadPercolatorAccount.into());
    }
    let d = ai.try_borrow_data()?;
    check_header(&d, KIND_PORTFOLIO)?;
    if d.len() != PORTFOLIO_ACCOUNT_LEN
        || rd_key(&d, PORTFOLIO_MARKET_OFF)? != *market
        || rd_key(&d, PORTFOLIO_SELF_OFF)? != *ai.key
        || rd_key(&d, PORTFOLIO_OWNER_OFF)? != *owner
    {
        return Err(VaultError::BadPercolatorAccount.into());
    }
    let control = rd_u64(&d, PORTFOLIO_MATCHER_CONTROL_OFF)?;
    Ok(PortfolioView {
        capital: rd_u128(&d, PORTFOLIO_CAPITAL_OFF)?,
        pnl: rd_i128(&d, PORTFOLIO_PNL_OFF)?,
        fee_credits: rd_i128(&d, PORTFOLIO_FEE_CREDITS_OFF)?,
        flat: rd_u64(&d, PORTFOLIO_ACTIVE_BITMAP_OFF)? == 0,
        portfolio_id: rd_u64(&d, PORTFOLIO_ID_OFF)?,
        sequence: rd_u64(&d, PORTFOLIO_SEQUENCE_OFF)?,
        position_epoch: (control & POSITION_EPOCH_MASK) >> POSITION_EPOCH_SHIFT,
    })
}

/// Percolator's matcher delegate PDA for an LP portfolio. Only Percolator can sign for it, so a
/// matcher call signed by it is a call from Percolator on behalf of that portfolio.
pub fn matcher_delegate(
    market: &Pubkey,
    lp_portfolio: &Pubkey,
    lp_owner: &Pubkey,
    matcher_program: &Pubkey,
    matcher_context: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"matcher",
            market.as_ref(),
            lp_portfolio.as_ref(),
            lp_owner.as_ref(),
            matcher_program.as_ref(),
            matcher_context.as_ref(),
        ],
        &PERCOLATOR_PROGRAM_ID,
    )
    .0
}

pub fn vault_authority(market: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"vault", market.as_ref()], &PERCOLATOR_PROGRAM_ID).0
}

// ---- instruction builders ----

pub fn init_portfolio(owner: &Pubkey, market: &Pubkey, portfolio: &Pubkey) -> Instruction {
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
        ],
        data: vec![TAG_INIT_PORTFOLIO],
    }
}

#[allow(clippy::too_many_arguments)]
pub fn deposit(
    owner: &Pubkey,
    market: &Pubkey,
    portfolio: &Pubkey,
    source: &Pubkey,
    percolator_vault: &Pubkey,
    portfolio_id: u64,
    sequence: u64,
    amount: u128,
) -> Instruction {
    let mut data = vec![TAG_DEPOSIT];
    data.extend_from_slice(&portfolio_id.to_le_bytes());
    data.extend_from_slice(&sequence.to_le_bytes());
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
            AccountMeta::new(*source, false),
            AccountMeta::new(*percolator_vault, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn withdraw(
    owner: &Pubkey,
    market: &Pubkey,
    portfolio: &Pubkey,
    dest: &Pubkey,
    percolator_vault: &Pubkey,
    percolator_vault_authority: &Pubkey,
    portfolio_id: u64,
    sequence: u64,
    amount: u128,
) -> Instruction {
    let mut data = vec![TAG_WITHDRAW];
    data.extend_from_slice(&portfolio_id.to_le_bytes());
    data.extend_from_slice(&sequence.to_le_bytes());
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new(*percolator_vault, false),
            AccountMeta::new_readonly(*percolator_vault_authority, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data,
    }
}

pub fn sync_maintenance_fee(market: &Pubkey, portfolio: &Pubkey, now_slot: u64) -> Instruction {
    let mut data = vec![TAG_SYNC_MAINTENANCE_FEE];
    data.extend_from_slice(&now_slot.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
        ],
        data,
    }
}

pub fn convert_released_pnl(
    owner: &Pubkey,
    market: &Pubkey,
    portfolio: &Pubkey,
    portfolio_id: u64,
    position_epoch: u64,
) -> Instruction {
    let mut data = vec![TAG_CONVERT_RELEASED_PNL];
    data.extend_from_slice(&portfolio_id.to_le_bytes());
    data.extend_from_slice(&position_epoch.to_le_bytes());
    // Upper bound on the conversion; the engine converts whatever is currently released.
    data.extend_from_slice(&u128::MAX.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
        ],
        data,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn close_resolved(
    owner: &Pubkey,
    market: &Pubkey,
    portfolio: &Pubkey,
    dest: &Pubkey,
    percolator_vault: &Pubkey,
    percolator_vault_authority: &Pubkey,
) -> Instruction {
    let mut data = vec![TAG_CLOSE_RESOLVED];
    data.extend_from_slice(&0u128.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new(*percolator_vault, false),
            AccountMeta::new_readonly(*percolator_vault_authority, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn set_matcher_config(
    owner: &Pubkey,
    market: &Pubkey,
    portfolio: &Pubkey,
    matcher_program: &Pubkey,
    matcher_context: &Pubkey,
    delegate: &Pubkey,
    view: &PortfolioView,
    asset_generation_frontier: u64,
    trade_fee_cap_bps: u16,
    expiry_slot: u64,
) -> Instruction {
    let mut data = vec![TAG_SET_MATCHER_CONFIG];
    data.extend_from_slice(&view.portfolio_id.to_le_bytes());
    data.extend_from_slice(&view.sequence.to_le_bytes());
    data.extend_from_slice(&view.position_epoch.to_le_bytes());
    data.extend_from_slice(&asset_generation_frontier.to_le_bytes());
    data.push(1);
    data.extend_from_slice(&trade_fee_cap_bps.to_le_bytes());
    data.extend_from_slice(&expiry_slot.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new_readonly(*market, false),
            AccountMeta::new(*portfolio, false),
            AccountMeta::new_readonly(*matcher_program, false),
            AccountMeta::new_readonly(*matcher_context, false),
            AccountMeta::new_readonly(*delegate, false),
        ],
        data,
    }
}

/// Permissionless activation of a new asset. The signer becomes the asset's `asset_admin`; the
/// four domain authorities are set explicitly.
#[allow(clippy::too_many_arguments)]
pub fn activate_asset(
    authority: &Pubkey,
    market: &Pubkey,
    fee_source: &Pubkey,
    percolator_vault: &Pubkey,
    asset_index: u16,
    market_id: u64,
    authority_epoch: u64,
    now_slot: u64,
    initial_price: u64,
    max_init_fee: u128,
    domain_authority: &Pubkey,
) -> Instruction {
    let mut data = vec![TAG_UPDATE_ASSET_LIFECYCLE, ASSET_ACTION_ACTIVATE];
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&market_id.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    data.extend_from_slice(&now_slot.to_le_bytes());
    data.extend_from_slice(&initial_price.to_le_bytes());
    data.extend_from_slice(&max_init_fee.to_le_bytes());
    for _ in 0..4 {
        data.extend_from_slice(domain_authority.as_ref());
    }
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*authority, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*fee_source, false),
            AccountMeta::new(*percolator_vault, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data,
    }
}

pub struct HybridOracle {
    pub leg_count: u8,
    pub leg_flags: u8,
    pub max_staleness_secs: u64,
    pub soft_stale_slots: u64,
    pub ewma_halflife_slots: u64,
    pub mark_min_fee: u64,
    pub invert: u8,
    pub unit_scale: u32,
    pub conf_filter_bps: u16,
    pub feeds: [[u8; 32]; 3],
}

/// Puts an asset in authority-mark mode: its mark is exactly what the oracle authority pushes.
/// `push` selects `PushAuthMark` (move the mark) instead of `ConfigureAuthMark` (set it up).
#[allow(clippy::too_many_arguments)]
pub fn auth_mark(
    push: bool,
    authority: &Pubkey,
    market: &Pubkey,
    asset_index: u16,
    market_id: u64,
    now_slot: u64,
    mark_e6: u64,
    observation_sequence: u64,
    authority_epoch: u64,
) -> Instruction {
    let mut data = vec![if push { TAG_PUSH_AUTH_MARK } else { TAG_CONFIGURE_AUTH_MARK }];
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&market_id.to_le_bytes());
    data.extend_from_slice(&now_slot.to_le_bytes());
    data.extend_from_slice(&mark_e6.to_le_bytes());
    data.extend_from_slice(&observation_sequence.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![AccountMeta::new_readonly(*authority, true), AccountMeta::new(*market, false)],
        data,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn configure_hybrid_oracle(
    authority: &Pubkey,
    market: &Pubkey,
    oracle_accounts: &[Pubkey],
    asset_index: u16,
    market_id: u64,
    now_slot: u64,
    now_unix_ts: i64,
    o: &HybridOracle,
    observation_sequence: u64,
    authority_epoch: u64,
) -> Instruction {
    let mut data = vec![TAG_CONFIGURE_HYBRID_ORACLE];
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&market_id.to_le_bytes());
    data.extend_from_slice(&now_slot.to_le_bytes());
    data.extend_from_slice(&now_unix_ts.to_le_bytes());
    data.push(o.leg_count);
    data.push(o.leg_flags);
    data.extend_from_slice(&o.max_staleness_secs.to_le_bytes());
    data.extend_from_slice(&o.soft_stale_slots.to_le_bytes());
    data.extend_from_slice(&o.ewma_halflife_slots.to_le_bytes());
    data.extend_from_slice(&o.mark_min_fee.to_le_bytes());
    data.push(o.invert);
    data.extend_from_slice(&o.unit_scale.to_le_bytes());
    data.extend_from_slice(&o.conf_filter_bps.to_le_bytes());
    for f in &o.feeds {
        data.extend_from_slice(f);
    }
    data.extend_from_slice(&observation_sequence.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    let mut accounts = vec![
        AccountMeta::new_readonly(*authority, true),
        AccountMeta::new(*market, false),
    ];
    accounts.extend(oracle_accounts.iter().map(|k| AccountMeta::new_readonly(*k, false)));
    Instruction { program_id: PERCOLATOR_PROGRAM_ID, accounts, data }
}

#[allow(clippy::too_many_arguments)]
pub fn withdraw_insurance_asset(
    operator: &Pubkey,
    market: &Pubkey,
    dest: &Pubkey,
    percolator_vault: &Pubkey,
    percolator_vault_authority: &Pubkey,
    asset_index: u16,
    market_id: u64,
    authority_epoch: u64,
    amount: u128,
) -> Instruction {
    let mut data = vec![TAG_WITHDRAW_INSURANCE_ASSET];
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&market_id.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*operator, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*dest, false),
            AccountMeta::new(*percolator_vault, false),
            AccountMeta::new_readonly(*percolator_vault_authority, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data,
    }
}

/// Retires an empty asset so its slot can be reused. Percolator gates retirement on the
/// market-level authority (`marketauth`) and refuses unless the asset holds no positions,
/// obligations or unspent insurance history it cannot clear.
pub fn retire_asset(
    marketauth: &Pubkey,
    market: &Pubkey,
    asset_index: u16,
    market_id: u64,
    authority_epoch: u64,
    now_slot: u64,
) -> Instruction {
    let mut data = vec![TAG_UPDATE_ASSET_LIFECYCLE, ASSET_ACTION_RETIRE];
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&market_id.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    data.extend_from_slice(&now_slot.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // initial_price: unused, must be zero
    data.extend_from_slice(&0u128.to_le_bytes()); // max_init_fee: unused
    data.extend_from_slice(&[0u8; 128]); // domain authorities: unused
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![AccountMeta::new_readonly(*marketauth, true), AccountMeta::new(*market, false)],
        data,
    }
}

/// Rotates the market-level authority. Both the current and the new authority sign.
pub fn update_market_authority(current: &Pubkey, new: &Pubkey, market: &Pubkey, authority_epoch: u64) -> Instruction {
    let mut data = vec![TAG_UPDATE_AUTHORITY];
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    data.extend_from_slice(new.as_ref());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*current, true),
            AccountMeta::new_readonly(*new, true),
            AccountMeta::new(*market, false),
        ],
        data,
    }
}

/// A taker trade against an LP portfolio whose matcher is `matcher_program` (the vault).
#[allow(clippy::too_many_arguments)]
pub fn trade_cpi(
    taker: &Pubkey,
    market: &Pubkey,
    taker_portfolio: &Pubkey,
    lp_portfolio: &Pubkey,
    matcher_program: &Pubkey,
    matcher_context: &Pubkey,
    matcher_delegate: &Pubkey,
    taker_view: &PortfolioView,
    lp_view: &PortfolioView,
    asset_index: u16,
    market_id: u64,
    size_q: i128,
    fee_bps: u64,
) -> Instruction {
    let mut data = vec![TAG_TRADE_CPI];
    data.extend_from_slice(&taker_view.portfolio_id.to_le_bytes());
    data.extend_from_slice(&taker_view.position_epoch.to_le_bytes());
    data.extend_from_slice(&lp_view.portfolio_id.to_le_bytes());
    data.extend_from_slice(&lp_view.position_epoch.to_le_bytes());
    data.extend_from_slice(&lp_view.sequence.to_le_bytes());
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&market_id.to_le_bytes());
    data.extend_from_slice(&size_q.to_le_bytes());
    data.extend_from_slice(&fee_bps.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // limit price: none, the fill is at the mark
    data.extend_from_slice(&0u16.to_le_bytes()); // backing fee cap
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*taker, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*taker_portfolio, false),
            AccountMeta::new(*lp_portfolio, false),
            AccountMeta::new_readonly(*matcher_program, false),
            AccountMeta::new(*matcher_context, false),
            AccountMeta::new_readonly(*matcher_delegate, false),
        ],
        data,
    }
}

/// Owner-signed unilateral reduction of one leg toward zero, at the engine's effective price and
/// within its unilateral close capacity. Never over-closes.
pub fn rebalance_reduce(
    owner: &Pubkey,
    market: &Pubkey,
    portfolio: &Pubkey,
    portfolio_id: u64,
    position_epoch: u64,
    asset_index: u16,
    reduce_q: u128,
) -> Instruction {
    let mut data = vec![TAG_REBALANCE_REDUCE];
    data.extend_from_slice(&portfolio_id.to_le_bytes());
    data.extend_from_slice(&position_epoch.to_le_bytes());
    data.extend_from_slice(&asset_index.to_le_bytes());
    data.extend_from_slice(&reduce_q.to_le_bytes());
    Instruction {
        program_id: PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new_readonly(*owner, true),
            AccountMeta::new(*market, false),
            AccountMeta::new(*portfolio, false),
        ],
        data,
    }
}
