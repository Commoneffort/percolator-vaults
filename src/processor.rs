//! Vault instructions. None of them has an admin: every parameter is fixed when the vault is
//! created, and every maintenance step (rolling an epoch, renewing the matcher approval,
//! converting released profit, settling after market resolution, sweeping stray tokens) can be
//! called by anyone.

use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    program::{invoke, invoke_signed},
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::Sysvar,
};
use spl_token::state::{Account as TokenAccount, AccountState, Mint};

use crate::{
    error::VaultError,
    math,
    percolator::{self as perc, PERCOLATOR_PROGRAM_ID},
    state::{self, *},
};

// ---- parameter bounds ----
pub const MAX_SPREAD_BPS: u16 = 5_000;
pub const MIN_EPOCH_LEN_SLOTS: u64 = 10;
pub const MAX_EPOCH_LEN_SLOTS: u64 = 7 * 216_000; // about a week of 400 ms slots
pub const MIN_MATCHER_TTL_SLOTS: u64 = 100;
pub const MAX_MATCHER_TTL_SLOTS: u64 = 30 * 216_000;
pub const MAX_ORACLE_STALENESS_SECS: u64 = 300;
/// A vault may hold at most 5x its NAV in notional. Percolator's margin rules apply on top.
pub const MAX_POSITION_NAV_BPS: u16 = 50_000;

// ---- canonical (operate-mode) vault template ----
// Operate-mode vaults are unique per (market, feed), so whoever creates one must not be able to
// choose parameters that cripple it. They all get this template; the creator only picks the feed.
#[cfg(feature = "devnet")]
pub const CANON_EPOCH_LEN_SLOTS: u64 = 1_500; // about ten minutes
#[cfg(not(feature = "devnet"))]
pub const CANON_EPOCH_LEN_SLOTS: u64 = 20;
#[cfg(feature = "devnet")]
pub const CANON_INSURANCE_FLOOR: u64 = 100_000_000; // 100 units of a 6-decimal collateral
#[cfg(not(feature = "devnet"))]
pub const CANON_INSURANCE_FLOOR: u64 = 10_000;
/// Share of the vault's value kept as cash outside Percolator when it redeploys at a roll, so
/// that ordinary withdrawals can be paid without closing the vault's position first.
pub const RESERVE_BPS: u64 = 1_000;
pub const CANON_POSITION_NAV_BPS: u16 = 30_000; // position up to 3x the vault's NAV
pub const CANON_FILL_NAV_BPS: u16 = 7_500; // each fill up to 0.75x the vault's NAV
#[cfg(feature = "devnet")]
pub const CANON_MATCHER_TTL_SLOTS: u64 = 216_000;
#[cfg(not(feature = "devnet"))]
pub const CANON_MATCHER_TTL_SLOTS: u64 = 200; // short in tests, so expiry and renewal can be exercised
pub const UNCAPPED: u128 = (i128::MAX as u128) / 4;
/// Share of a canonical market's harvested fees paid to whoever opened it (Hyperliquid HIP-3
/// style). It carries no powers: the opener can only claim what has accrued.
pub const OPENER_FEE_BPS: u64 = 1_000;

/// The opener's cut of `amount` of harvested fees for vault `v`.
pub fn opener_cut(v: &VaultState, amount: u64) -> u64 {
    if v.vault_kind == KIND_CANONICAL {
        ((amount as u128 * OPENER_FEE_BPS as u128) / 10_000) as u64
    } else {
        0
    }
}

impl InitParams {
    /// The fixed template for canonical vaults: only the seed-free feed choice and the listing
    /// fee budget come from the caller.
    pub fn canonical(feed: [u8; 32], listing_fee_max: u64, frontier: u64) -> Self {
        Self {
            seed: 0,
            asset_index: 0,
            spread_bps: 10,
            unwind_spread_bps: 0,
            trade_fee_cap_bps: 10_000,
            backing_fee_cap_bps: 0,
            max_fill_abs: UNCAPPED,
            max_inventory_abs: UNCAPPED,
            epoch_len_slots: CANON_EPOCH_LEN_SLOTS,
            matcher_ttl_slots: CANON_MATCHER_TTL_SLOTS,
            asset_generation_frontier: frontier,
            mode: MODE_OPERATE,
            insurance_floor: CANON_INSURANCE_FLOOR,
            listing_fee_max,
            oracle_leg_count: 1,
            oracle_leg_flags: 0,
            oracle_invert: 0,
            oracle_unit_scale: 0,
            oracle_conf_filter_bps: 0,
            oracle_max_staleness_secs: MAX_ORACLE_STALENESS_SECS,
            oracle_soft_stale_slots: 200,
            oracle_ewma_halflife_slots: 1,
            oracle_mark_min_fee: 0,
            oracle_feeds: [feed, [0; 32], [0; 32]],
            position_nav_bps: CANON_POSITION_NAV_BPS,
            fill_nav_bps: CANON_FILL_NAV_BPS,
        }
    }
}

pub const TAG_INIT_VAULT: u8 = 16;
pub const TAG_REQUEST_DEPOSIT: u8 = 17;
pub const TAG_REQUEST_WITHDRAW: u8 = 18;
pub const TAG_ROLL_EPOCH: u8 = 19;
pub const TAG_CLAIM: u8 = 20;
pub const TAG_REFRESH_MATCHER: u8 = 21;
pub const TAG_CONVERT_PNL: u8 = 22;
pub const TAG_SETTLE_RESOLVED: u8 = 23;
pub const TAG_REDEEM_TERMINAL: u8 = 24;
pub const TAG_SWEEP: u8 = 25;
pub const TAG_LIST_ASSET: u8 = 26;
pub const TAG_HARVEST_FEES: u8 = 27;
pub const TAG_UNWIND: u8 = 28;
pub const TAG_CLAIM_OPENER_FEES: u8 = 29;
pub const TAG_ACCEPT_GOVERNANCE: u8 = 30;
pub const TAG_RETIRE_MARKET: u8 = 31;
pub const TAG_PUSH_MARK: u8 = 32;
pub const TAG_ARM_FILL: u8 = 33;
pub const TAG_SYNC_INVENTORY: u8 = 34;
pub const TAG_REQUIRE_FLAT: u8 = 35;

/// A market with no liquidity providers can be retired this many epochs after it was listed.
pub const RETIRE_IDLE_EPOCHS: u64 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitParams {
    pub seed: u64,
    /// Attach mode: the existing asset to quote. Operate mode: ignored (chosen at listing).
    pub asset_index: u16,
    pub spread_bps: u16,
    pub unwind_spread_bps: u16,
    pub trade_fee_cap_bps: u16,
    pub backing_fee_cap_bps: u16,
    pub max_fill_abs: u128,
    pub max_inventory_abs: u128,
    pub epoch_len_slots: u64,
    pub matcher_ttl_slots: u64,
    pub asset_generation_frontier: u64,
    pub mode: u8,
    pub insurance_floor: u64,
    pub listing_fee_max: u64,
    pub oracle_leg_count: u8,
    pub oracle_leg_flags: u8,
    pub oracle_invert: u8,
    pub oracle_unit_scale: u32,
    pub oracle_conf_filter_bps: u16,
    pub oracle_max_staleness_secs: u64,
    pub oracle_soft_stale_slots: u64,
    pub oracle_ewma_halflife_slots: u64,
    pub oracle_mark_min_fee: u64,
    pub oracle_feeds: [[u8; 32]; 3],
    pub position_nav_bps: u16,
    pub fill_nav_bps: u16,
}

impl InitParams {
    pub const LEN: usize = 8 + 2 * 5 + 16 * 2 + 8 * 3 + 1 + 8 * 2 + 3 + 4 + 2 + 8 * 4 + 96 + 4;

    pub fn encode(&self) -> Vec<u8> {
        let mut d = vec![TAG_INIT_VAULT];
        d.extend_from_slice(&self.seed.to_le_bytes());
        d.extend_from_slice(&self.asset_index.to_le_bytes());
        d.extend_from_slice(&self.spread_bps.to_le_bytes());
        d.extend_from_slice(&self.unwind_spread_bps.to_le_bytes());
        d.extend_from_slice(&self.trade_fee_cap_bps.to_le_bytes());
        d.extend_from_slice(&self.backing_fee_cap_bps.to_le_bytes());
        d.extend_from_slice(&self.max_fill_abs.to_le_bytes());
        d.extend_from_slice(&self.max_inventory_abs.to_le_bytes());
        d.extend_from_slice(&self.epoch_len_slots.to_le_bytes());
        d.extend_from_slice(&self.matcher_ttl_slots.to_le_bytes());
        d.extend_from_slice(&self.asset_generation_frontier.to_le_bytes());
        d.push(self.mode);
        d.extend_from_slice(&self.insurance_floor.to_le_bytes());
        d.extend_from_slice(&self.listing_fee_max.to_le_bytes());
        d.push(self.oracle_leg_count);
        d.push(self.oracle_leg_flags);
        d.push(self.oracle_invert);
        d.extend_from_slice(&self.oracle_unit_scale.to_le_bytes());
        d.extend_from_slice(&self.oracle_conf_filter_bps.to_le_bytes());
        d.extend_from_slice(&self.oracle_max_staleness_secs.to_le_bytes());
        d.extend_from_slice(&self.oracle_soft_stale_slots.to_le_bytes());
        d.extend_from_slice(&self.oracle_ewma_halflife_slots.to_le_bytes());
        d.extend_from_slice(&self.oracle_mark_min_fee.to_le_bytes());
        for f in &self.oracle_feeds {
            d.extend_from_slice(f);
        }
        d.extend_from_slice(&self.position_nav_bps.to_le_bytes());
        d.extend_from_slice(&self.fill_nav_bps.to_le_bytes());
        d
    }

    pub fn decode(d: &[u8]) -> Result<Self, VaultError> {
        if d.len() != Self::LEN {
            return Err(VaultError::InvalidInstruction);
        }
        let mut r = Reader(d);
        Ok(Self {
            seed: r.u64(),
            asset_index: r.u16(),
            spread_bps: r.u16(),
            unwind_spread_bps: r.u16(),
            trade_fee_cap_bps: r.u16(),
            backing_fee_cap_bps: r.u16(),
            max_fill_abs: r.u128(),
            max_inventory_abs: r.u128(),
            epoch_len_slots: r.u64(),
            matcher_ttl_slots: r.u64(),
            asset_generation_frontier: r.u64(),
            mode: r.u8(),
            insurance_floor: r.u64(),
            listing_fee_max: r.u64(),
            oracle_leg_count: r.u8(),
            oracle_leg_flags: r.u8(),
            oracle_invert: r.u8(),
            oracle_unit_scale: r.u32(),
            oracle_conf_filter_bps: r.u16(),
            oracle_max_staleness_secs: r.u64(),
            oracle_soft_stale_slots: r.u64(),
            oracle_ewma_halflife_slots: r.u64(),
            oracle_mark_min_fee: r.u64(),
            oracle_feeds: [r.take(), r.take(), r.take()],
            position_nav_bps: r.u16(),
            fill_nav_bps: r.u16(),
        })
    }

    pub fn validate(&self) -> Result<(), VaultError> {
        let common = self.spread_bps >= 1
            && self.spread_bps <= MAX_SPREAD_BPS
            && self.unwind_spread_bps <= self.spread_bps
            && self.trade_fee_cap_bps <= 10_000
            && self.backing_fee_cap_bps <= 10_000
            && self.max_fill_abs > 0
            && self.max_fill_abs <= i128::MAX as u128
            && self.max_inventory_abs > 0
            && self.max_inventory_abs <= (i128::MAX as u128) / 4
            && (MIN_EPOCH_LEN_SLOTS..=MAX_EPOCH_LEN_SLOTS).contains(&self.epoch_len_slots)
            && (MIN_MATCHER_TTL_SLOTS..=MAX_MATCHER_TTL_SLOTS).contains(&self.matcher_ttl_slots)
            && self.position_nav_bps <= MAX_POSITION_NAV_BPS
            && self.fill_nav_bps <= self.position_nav_bps;
        let mode_ok = match self.mode {
            // Attach mode quoted on assets whose price the vault does not control, so their
            // fills could not be sequenced by the router and could be front-run. Not offered.
            MODE_ATTACH => false,
            // Percolator validates the oracle parameters themselves when the asset is listed.
            // A long staleness window would let takers trade against an old price, so it is
            // bounded here as well as by Percolator.
            MODE_OPERATE => {
                (1..=3).contains(&self.oracle_leg_count)
                    && (1..=MAX_ORACLE_STALENESS_SECS).contains(&self.oracle_max_staleness_secs)
            }
            _ => false,
        };
        if common && mode_ok {
            Ok(())
        } else {
            Err(VaultError::InvalidParams)
        }
    }
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let (a, b) = self.0.split_at(N);
        self.0 = b;
        a.try_into().unwrap()
    }
    fn u8(&mut self) -> u8 {
        u8::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
    fn u128(&mut self) -> u128 {
        u128::from_le_bytes(self.take())
    }
}

fn arg_u64(d: &[u8]) -> Result<u64, VaultError> {
    if d.len() != 8 {
        return Err(VaultError::InvalidInstruction);
    }
    Ok(u64::from_le_bytes(d.try_into().unwrap()))
}

fn no_args(d: &[u8]) -> Result<(), VaultError> {
    if d.is_empty() {
        Ok(())
    } else {
        Err(VaultError::InvalidInstruction)
    }
}

// ---- account checks ----

fn acc<'a, 'b>(accounts: &'a [AccountInfo<'b>], i: usize) -> Result<&'a AccountInfo<'b>, VaultError> {
    accounts.get(i).ok_or(VaultError::BadAccount)
}
fn signer(ai: &AccountInfo) -> Result<(), VaultError> {
    if ai.is_signer {
        Ok(())
    } else {
        Err(VaultError::NotSigner)
    }
}
fn writable(ai: &AccountInfo) -> Result<(), VaultError> {
    if ai.is_writable {
        Ok(())
    } else {
        Err(VaultError::NotWritable)
    }
}
fn key_is(ai: &AccountInfo, k: &Pubkey) -> Result<(), VaultError> {
    if ai.key == k {
        Ok(())
    } else {
        Err(VaultError::BadAccount)
    }
}
fn programs(
    percolator: &AccountInfo,
    token: &AccountInfo,
) -> Result<(), VaultError> {
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    key_is(token, &spl_token::ID)
}

fn token_account(ai: &AccountInfo) -> Result<TokenAccount, VaultError> {
    if ai.owner != &spl_token::ID {
        return Err(VaultError::BadAccount);
    }
    let d = ai.try_borrow_data().map_err(|_| VaultError::BadAccount)?;
    TokenAccount::unpack(&d).map_err(|_| VaultError::BadAccount)
}
fn token_amount(ai: &AccountInfo) -> Result<u64, VaultError> {
    Ok(token_account(ai)?.amount)
}
fn mint_supply(ai: &AccountInfo) -> Result<u64, VaultError> {
    if ai.owner != &spl_token::ID {
        return Err(VaultError::BadAccount);
    }
    let d = ai.try_borrow_data().map_err(|_| VaultError::BadAccount)?;
    Ok(Mint::unpack(&d).map_err(|_| VaultError::BadAccount)?.supply)
}

fn add(a: u64, b: u64) -> Result<u64, VaultError> {
    a.checked_add(b).ok_or(VaultError::Overflow)
}
fn sub(a: u64, b: u64) -> Result<u64, VaultError> {
    a.checked_sub(b).ok_or(VaultError::Overflow)
}

/// Signer seeds for the vault PDA, which owns the portfolio, the buffer, the escrow and the
/// share mint authority.
macro_rules! vault_seeds {
    ($v:expr, $bump:expr) => {
        &[
            if $v.vault_kind == KIND_CANONICAL { SEED_CANONICAL } else { SEED_VAULT },
            $v.market.as_ref(),
            if $v.vault_kind == KIND_CANONICAL { &$v.oracle_feeds[0][..] } else { $v.creator.as_ref() },
            &$v.seed.to_le_bytes(),
            &[$bump],
        ]
    };
}

#[allow(clippy::too_many_arguments)]
fn create_pda_account<'a>(
    payer: &AccountInfo<'a>,
    target: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    owner: &Pubkey,
    space: usize,
    seeds: &[&[u8]],
) -> ProgramResult {
    // Refuse to adopt an account someone pre-created or pre-funded with data or a foreign owner.
    if target.owner != &system_program::ID || target.data_len() != 0 {
        return Err(VaultError::AlreadyInitialized.into());
    }
    let rent = Rent::get()?.minimum_balance(space);
    let have = target.lamports();
    if have == 0 {
        invoke_signed(
            &system_instruction::create_account(payer.key, target.key, rent, space as u64, owner),
            &[payer.clone(), target.clone(), system.clone()],
            &[seeds],
        )
    } else {
        // Someone sent lamports to the address first. Top up, allocate and assign instead of
        // create_account (which would fail), so a dust transfer cannot block creation.
        if have < rent {
            invoke(
                &system_instruction::transfer(payer.key, target.key, rent - have),
                &[payer.clone(), target.clone(), system.clone()],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(target.key, space as u64),
            &[target.clone(), system.clone()],
            &[seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(target.key, owner),
            &[target.clone(), system.clone()],
            &[seeds],
        )
    }
}

fn check_vault_token_account(
    ai: &AccountInfo,
    expected_key: &Pubkey,
    mint: &Pubkey,
    vault: &Pubkey,
) -> Result<(), VaultError> {
    key_is(ai, expected_key)?;
    let t = token_account(ai)?;
    if t.mint != *mint
        || t.owner != *vault
        || t.state != AccountState::Initialized
        || t.delegate.is_some()
        || t.close_authority.is_some()
    {
        return Err(VaultError::BadAccount);
    }
    Ok(())
}

pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (tag, rest) = data.split_first().ok_or(VaultError::InvalidInstruction)?;
    match *tag {
        TAG_INIT_VAULT => init_vault(program_id, accounts, InitParams::decode(rest)?),
        TAG_REQUEST_DEPOSIT => request_deposit(program_id, accounts, arg_u64(rest)?),
        TAG_REQUEST_WITHDRAW => request_withdraw(program_id, accounts, arg_u64(rest)?),
        TAG_ROLL_EPOCH => roll_epoch(program_id, accounts, arg_u64(rest)?),
        TAG_CLAIM => {
            no_args(rest)?;
            claim(program_id, accounts)
        }
        TAG_REFRESH_MATCHER => refresh_matcher(program_id, accounts, arg_u64(rest)?),
        TAG_CONVERT_PNL => {
            no_args(rest)?;
            convert_pnl(program_id, accounts)
        }
        TAG_SETTLE_RESOLVED => {
            no_args(rest)?;
            settle_resolved(program_id, accounts)
        }
        TAG_REDEEM_TERMINAL => redeem_terminal(program_id, accounts, arg_u64(rest)?),
        TAG_SWEEP => {
            no_args(rest)?;
            sweep(program_id, accounts)
        }
        TAG_UNWIND => {
            no_args(rest)?;
            unwind(program_id, accounts)
        }
        TAG_CLAIM_OPENER_FEES => {
            no_args(rest)?;
            claim_opener_fees(program_id, accounts)
        }
        TAG_LIST_ASSET => list_asset(program_id, accounts, ListArgs::decode(rest)?),
        TAG_ACCEPT_GOVERNANCE => accept_governance(program_id, accounts, arg_u64(rest)?),
        TAG_PUSH_MARK => {
            if rest.len() != 24 {
                return Err(VaultError::InvalidInstruction.into());
            }
            let r = |i: usize| u64::from_le_bytes(rest[i..i + 8].try_into().unwrap());
            push_mark(program_id, accounts, r(0), r(8), r(16))
        }
        TAG_ARM_FILL => {
            if rest.len() != 16 {
                return Err(VaultError::InvalidInstruction.into());
            }
            arm_fill(program_id, accounts, i128::from_le_bytes(rest.try_into().unwrap()))
        }
        TAG_REQUIRE_FLAT => {
            no_args(rest)?;
            require_flat(program_id, accounts)
        }
        TAG_SYNC_INVENTORY => {
            no_args(rest)?;
            sync_inventory(program_id, accounts)
        }
        TAG_RETIRE_MARKET => {
            if rest.len() != 16 {
                return Err(VaultError::InvalidInstruction.into());
            }
            retire_market(
                program_id,
                accounts,
                u64::from_le_bytes(rest[..8].try_into().unwrap()),
                u64::from_le_bytes(rest[8..].try_into().unwrap()),
            )
        }
        TAG_HARVEST_FEES => {
            if rest.len() != 16 {
                return Err(VaultError::InvalidInstruction.into());
            }
            harvest_fees(
                program_id,
                accounts,
                u64::from_le_bytes(rest[..8].try_into().unwrap()),
                u64::from_le_bytes(rest[8..].try_into().unwrap()),
            )
        }
        _ => Err(VaultError::InvalidInstruction.into()),
    }
}

/// Creates a vault, its share mint, buffer and escrow, its Percolator LP portfolio, and
/// approves the vault program as that portfolio's matcher.
///
/// Accounts: 0 payer [s,w], 1 creator [s], 2 vault [w], 3 market [w], 4 collateral mint,
/// 5 share mint [w], 6 buffer [w], 7 share escrow [w], 8 LP portfolio [w], 9 matcher delegate,
/// 10 Percolator program, 11 this program, 12 token program, 13 system program.
fn init_vault(program_id: &Pubkey, accounts: &[AccountInfo], p: InitParams) -> ProgramResult {
    // Operate mode is canonical: one vault per (market, feed), on the fixed template. The caller
    // only chooses the feed (and how much listing fee it will pay).
    let p = if p.mode == MODE_OPERATE {
        InitParams::canonical(p.oracle_feeds[0], p.listing_fee_max, p.asset_generation_frontier)
    } else {
        p
    };
    p.validate()?;
    let payer = acc(accounts, 0)?;
    let creator = acc(accounts, 1)?;
    let vault_ai = acc(accounts, 2)?;
    let market = acc(accounts, 3)?;
    let collateral_mint = acc(accounts, 4)?;
    let share_mint = acc(accounts, 5)?;
    let buffer = acc(accounts, 6)?;
    let escrow = acc(accounts, 7)?;
    let portfolio = acc(accounts, 8)?;
    let delegate = acc(accounts, 9)?;
    let percolator = acc(accounts, 10)?;
    let this_program = acc(accounts, 11)?;
    let token = acc(accounts, 12)?;
    let system = acc(accounts, 13)?;

    signer(payer)?;
    signer(creator)?;
    for w in [payer, vault_ai, market, share_mint, buffer, escrow, portfolio] {
        writable(w)?;
    }
    programs(percolator, token)?;
    key_is(this_program, program_id)?;
    key_is(system, &system_program::ID)?;

    // The market must be a Percolator market, and the collateral its primary mint.
    let collateral = perc::market_collateral_mint(market)?;
    key_is(collateral_mint, &collateral)?;
    if collateral_mint.owner != &spl_token::ID {
        return Err(VaultError::BadAccount.into());
    }
    let collateral_decimals = {
        let d = collateral_mint.try_borrow_data()?;
        Mint::unpack(&d).map_err(|_| VaultError::BadAccount)?.decimals
    };
    let share_decimals = collateral_decimals
        .checked_add(3)
        .ok_or(VaultError::InvalidParams)?;

    let kind = if p.mode == MODE_OPERATE { KIND_CANONICAL } else { KIND_LEGACY };
    let (vault_key, vault_bump) = if kind == KIND_CANONICAL {
        canonical_vault_address(program_id, market.key, &p.oracle_feeds[0])
    } else {
        vault_address(program_id, market.key, creator.key, p.seed)
    };
    key_is(vault_ai, &vault_key)?;
    let (mint_key, mint_bump) = child_address(program_id, SEED_SHARES, &vault_key);
    let (buffer_key, buffer_bump) = child_address(program_id, SEED_BUFFER, &vault_key);
    let (escrow_key, escrow_bump) = child_address(program_id, SEED_ESCROW, &vault_key);
    let (portfolio_key, portfolio_bump) = child_address(program_id, SEED_PORTFOLIO, &vault_key);
    key_is(share_mint, &mint_key)?;
    key_is(buffer, &buffer_key)?;
    key_is(escrow, &escrow_key)?;
    key_is(portfolio, &portfolio_key)?;
    let delegate_key =
        perc::matcher_delegate(market.key, &portfolio_key, &vault_key, program_id, &vault_key);
    key_is(delegate, &delegate_key)?;

    let seed_bytes = p.seed.to_le_bytes();
    let vault_signer: &[&[u8]] = &[
        if kind == KIND_CANONICAL { SEED_CANONICAL } else { SEED_VAULT },
        market.key.as_ref(),
        if kind == KIND_CANONICAL { &p.oracle_feeds[0][..] } else { creator.key.as_ref() },
        &seed_bytes,
        &[vault_bump],
    ];
    create_pda_account(payer, vault_ai, system, program_id, VAULT_ACCOUNT_LEN, vault_signer)?;
    create_pda_account(
        payer,
        share_mint,
        system,
        &spl_token::ID,
        Mint::LEN,
        &[SEED_SHARES, vault_key.as_ref(), &[mint_bump]],
    )?;
    invoke(
        &spl_token::instruction::initialize_mint2(
            &spl_token::ID,
            &mint_key,
            &vault_key,
            None,
            share_decimals,
        )?,
        &[share_mint.clone()],
    )?;
    create_pda_account(
        payer,
        buffer,
        system,
        &spl_token::ID,
        TokenAccount::LEN,
        &[SEED_BUFFER, vault_key.as_ref(), &[buffer_bump]],
    )?;
    invoke(
        &spl_token::instruction::initialize_account3(
            &spl_token::ID,
            &buffer_key,
            &collateral,
            &vault_key,
        )?,
        &[buffer.clone(), collateral_mint.clone()],
    )?;
    create_pda_account(
        payer,
        escrow,
        system,
        &spl_token::ID,
        TokenAccount::LEN,
        &[SEED_ESCROW, vault_key.as_ref(), &[escrow_bump]],
    )?;
    invoke(
        &spl_token::instruction::initialize_account3(&spl_token::ID, &escrow_key, &mint_key, &vault_key)?,
        &[escrow.clone(), share_mint.clone()],
    )?;
    create_pda_account(
        payer,
        portfolio,
        system,
        &PERCOLATOR_PROGRAM_ID,
        perc::PORTFOLIO_ACCOUNT_LEN,
        &[SEED_PORTFOLIO, vault_key.as_ref(), &[portfolio_bump]],
    )?;
    invoke_signed(
        &perc::init_portfolio(&vault_key, market.key, &portfolio_key),
        &[vault_ai.clone(), market.clone(), portfolio.clone(), percolator.clone()],
        &[vault_signer],
    )?;
    let view = perc::read_portfolio(portfolio, market.key, &vault_key)?;

    // Attach mode quotes an asset that already exists: record its generation now (the read also
    // proves the slot exists). Operate mode records it when it lists its own asset.
    let (status, asset_index, asset_market_id) = if p.mode == MODE_ATTACH {
        let (id, _) = perc::asset_insurance(market, p.asset_index)?;
        if id == 0 {
            return Err(VaultError::UnknownAsset.into());
        }
        (STATUS_ACTIVE, p.asset_index, id)
    } else {
        (STATUS_PENDING_LISTING, u16::MAX, 0)
    };

    let now = Clock::get()?.slot;
    let v = VaultState {
        magic: VAULT_MAGIC,
        version: 1,
        status,
        vault_bump,
        portfolio_bump,
        share_mint_bump: mint_bump,
        buffer_bump,
        escrow_bump,
        share_decimals,
        market: *market.key,
        creator: *creator.key,
        seed: p.seed,
        collateral_mint: collateral,
        share_mint: mint_key,
        buffer: buffer_key,
        share_escrow: escrow_key,
        lp_portfolio: portfolio_key,
        matcher_delegate: delegate_key,
        portfolio_id: view.portfolio_id,
        asset_index,
        spread_bps: p.spread_bps,
        unwind_spread_bps: p.unwind_spread_bps,
        trade_fee_cap_bps: p.trade_fee_cap_bps,
        backing_fee_cap_bps: p.backing_fee_cap_bps,
        position_nav_bps: p.position_nav_bps,
        fill_nav_bps: p.fill_nav_bps,
        _pad0: [0; 2],
        max_fill_abs: p.max_fill_abs,
        max_inventory_abs: p.max_inventory_abs,
        epoch_len_slots: p.epoch_len_slots,
        matcher_ttl_slots: p.matcher_ttl_slots,
        mode: p.mode,
        oracle_leg_count: p.oracle_leg_count,
        oracle_leg_flags: p.oracle_leg_flags,
        oracle_invert: p.oracle_invert,
        oracle_unit_scale: p.oracle_unit_scale,
        oracle_conf_filter_bps: p.oracle_conf_filter_bps,
        vault_kind: kind,
        needs_flat: 0,
        _pad1: [0; 4],
        oracle_max_staleness_secs: p.oracle_max_staleness_secs,
        oracle_soft_stale_slots: p.oracle_soft_stale_slots,
        oracle_ewma_halflife_slots: p.oracle_ewma_halflife_slots,
        oracle_mark_min_fee: p.oracle_mark_min_fee,
        oracle_feeds: p.oracle_feeds,
        insurance_floor: p.insurance_floor,
        listing_fee_max: p.listing_fee_max,
        asset_market_id,
        inventory: 0,
        epoch: 0,
        epoch_start_slot: now,
        pending_deposit_assets: 0,
        pending_withdraw_shares: 0,
        reserved_assets: 0,
        last_nav: 0,
        created_slot: now,
        total_fills: 0,
        total_fees_harvested: 0,
        opener_fees_owed: 0,
        opener_fees_total: 0,
        listed_slot: 0,
        armed_size: 0,
        armed_slot: 0,
        _reserved: [0; 8],
    };
    state::store_vault(vault_ai, &v)?;

    let expiry = now.checked_add(p.matcher_ttl_slots).ok_or(VaultError::Overflow)?;
    invoke_signed(
        &perc::set_matcher_config(
            &vault_key,
            market.key,
            &portfolio_key,
            program_id,
            &vault_key,
            &delegate_key,
            &view,
            p.asset_generation_frontier,
            p.trade_fee_cap_bps,
            expiry,
        ),
        &[
            vault_ai.clone(),
            market.clone(),
            portfolio.clone(),
            this_program.clone(),
            delegate.clone(),
            percolator.clone(),
        ],
        &[vault_signer],
    )?;
    Ok(())
}

/// Loads (or creates) the caller's ticket for the current epoch.
fn open_ticket<'a>(
    program_id: &Pubkey,
    v: &VaultState,
    vault_key: &Pubkey,
    user: &AccountInfo<'a>,
    ticket: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
) -> Result<Ticket, solana_program::program_error::ProgramError> {
    let (ticket_key, bump) = ticket_address(program_id, vault_key, user.key);
    key_is(ticket, &ticket_key)?;
    if ticket.owner == &system_program::ID {
        key_is(system, &system_program::ID)?;
        create_pda_account(
            user,
            ticket,
            system,
            program_id,
            TICKET_LEN,
            &[SEED_TICKET, vault_key.as_ref(), user.key.as_ref(), &[bump]],
        )?;
        return Ok(Ticket {
            magic: TICKET_MAGIC,
            vault: *vault_key,
            owner: *user.key,
            epoch: v.epoch,
            deposit_assets: 0,
            withdraw_shares: 0,
            bump,
            _pad: [0; 7],
        });
    }
    let mut t = state::load_ticket(ticket, program_id)?;
    if t.vault != *vault_key || t.owner != *user.key {
        return Err(VaultError::BadAccount.into());
    }
    if t.epoch != v.epoch {
        if t.deposit_assets != 0 || t.withdraw_shares != 0 {
            return Err(VaultError::ClaimFirst.into());
        }
        t.epoch = v.epoch;
    }
    Ok(t)
}

/// Queues a deposit for the current epoch. Accounts: 0 user [s,w], 1 vault [w], 2 ticket [w],
/// 3 user collateral [w], 4 buffer [w], 5 token program, 6 system program.
fn request_deposit(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    if amount == 0 {
        return Err(VaultError::ZeroAmount.into());
    }
    let user = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let ticket = acc(accounts, 2)?;
    let source = acc(accounts, 3)?;
    let buffer = acc(accounts, 4)?;
    let token = acc(accounts, 5)?;
    let system = acc(accounts, 6)?;
    signer(user)?;
    for w in [user, vault_ai, ticket, source, buffer] {
        writable(w)?;
    }
    key_is(token, &spl_token::ID)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    let mut t = open_ticket(program_id, &v, vault_ai.key, user, ticket, system)?;

    invoke(
        &spl_token::instruction::transfer(&spl_token::ID, source.key, buffer.key, user.key, &[], amount)?,
        &[source.clone(), buffer.clone(), user.clone(), token.clone()],
    )?;
    t.deposit_assets = add(t.deposit_assets, amount)?;
    v.pending_deposit_assets = add(v.pending_deposit_assets, amount)?;
    state::store_ticket(ticket, &t)?;
    state::store_vault(vault_ai, &v)
}

/// Queues a withdrawal: the shares move into escrow now and are priced at the next roll.
/// Accounts: 0 user [s,w], 1 vault [w], 2 ticket [w], 3 user shares [w], 4 share escrow [w],
/// 5 token program, 6 system program.
fn request_withdraw(program_id: &Pubkey, accounts: &[AccountInfo], shares: u64) -> ProgramResult {
    if shares == 0 {
        return Err(VaultError::ZeroAmount.into());
    }
    let user = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let ticket = acc(accounts, 2)?;
    let source = acc(accounts, 3)?;
    let escrow = acc(accounts, 4)?;
    let token = acc(accounts, 5)?;
    let system = acc(accounts, 6)?;
    signer(user)?;
    for w in [user, vault_ai, ticket, source, escrow] {
        writable(w)?;
    }
    key_is(token, &spl_token::ID)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    check_vault_token_account(escrow, &v.share_escrow, &v.share_mint, vault_ai.key)?;
    let mut t = open_ticket(program_id, &v, vault_ai.key, user, ticket, system)?;

    invoke(
        &spl_token::instruction::transfer(&spl_token::ID, source.key, escrow.key, user.key, &[], shares)?,
        &[source.clone(), escrow.clone(), user.clone(), token.clone()],
    )?;
    t.withdraw_shares = add(t.withdraw_shares, shares)?;
    v.pending_withdraw_shares = add(v.pending_withdraw_shares, shares)?;
    state::store_ticket(ticket, &t)?;
    state::store_vault(vault_ai, &v)
}

/// Settles the epoch once it is over: prices every queued withdrawal and deposit at one net
/// asset value, then redeploys the free collateral (keeping `RESERVE_BPS` of it as cash).
///
/// With the LP portfolio flat anyone can call it: a flat vault's value does not depend on the
/// price. With a position open the value does, so only the router can (account 15, its
/// authority, as signer): it calls this from `Settle`, at the first Pyth price published after a
/// target time fixed in advance, with Percolator's price at it. The position must be settled at
/// that price, nothing is withdrawn from Percolator, and the epoch's withdrawals must fit in
/// the vault's cash; if they do not, the roll fails and `RequireFlat` applies.
///
/// Accounts: 0 cranker [s,w], 1 vault [w], 2 market [w], 3 LP portfolio [w], 4 buffer [w],
/// 5 Percolator collateral vault [w], 6 Percolator vault authority, 7 share mint [w],
/// 8 share escrow [w], 9 epoch record [w], 10 matcher delegate, 11 Percolator program,
/// 12 this program, 13 token program, 14 system program, 15 (open position only) the router's
/// authority [s].
/// Data: the market's current asset-generation frontier (checked by Percolator).
fn roll_epoch(program_id: &Pubkey, accounts: &[AccountInfo], frontier: u64) -> ProgramResult {
    let cranker = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let market = acc(accounts, 2)?;
    let portfolio = acc(accounts, 3)?;
    let buffer = acc(accounts, 4)?;
    let perc_vault = acc(accounts, 5)?;
    let perc_vault_auth = acc(accounts, 6)?;
    let share_mint = acc(accounts, 7)?;
    let escrow = acc(accounts, 8)?;
    let record_ai = acc(accounts, 9)?;
    let delegate = acc(accounts, 10)?;
    let percolator = acc(accounts, 11)?;
    let this_program = acc(accounts, 12)?;
    let token = acc(accounts, 13)?;
    let system = acc(accounts, 14)?;
    signer(cranker)?;
    for w in [cranker, vault_ai, market, portfolio, buffer, perc_vault, share_mint, escrow, record_ai] {
        writable(w)?;
    }
    programs(percolator, token)?;
    key_is(this_program, program_id)?;
    key_is(system, &system_program::ID)?;

    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    key_is(share_mint, &v.share_mint)?;
    key_is(delegate, &v.matcher_delegate)?;
    key_is(perc_vault_auth, &perc::vault_authority(&v.market))?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    check_vault_token_account(escrow, &v.share_escrow, &v.share_mint, vault_ai.key)?;
    let now = Clock::get()?.slot;
    if now < v.epoch_start_slot.saturating_add(v.epoch_len_slots) {
        return Err(VaultError::EpochNotOver.into());
    }
    let vault_key = *vault_ai.key;
    let bump = v.vault_bump;
    let signer_seeds: &[&[u8]] = vault_seeds!(v, bump);

    // 1. Settle fees and losses up to now, then prove the portfolio is flat.
    invoke(
        &perc::sync_maintenance_fee(market.key, portfolio.key, now),
        &[market.clone(), portfolio.clone(), percolator.clone()],
    )?;
    let view = perc::read_portfolio(portfolio, market.key, &vault_key)?;
    let open = !view.flat;
    if open {
        match accounts.get(15) {
            Some(router) => router_signed(router)?,
            None => return Err(VaultError::NotFlat.into()),
        }
        if perc::positions(&market.try_borrow_data()?, &portfolio.try_borrow_data()?)?.iter().any(|p| !p.settled) {
            return Err(VaultError::NotSettled.into());
        }
    }

    // 2. Flat: pull all capital back into the buffer. Percolator refuses unless the portfolio is
    //    flat with no loss outstanding; the amount actually received is measured, not assumed.
    //    Open: the capital stays where it is and is counted at Percolator's figure.
    let buffer_before = token_amount(buffer)?;
    if !open && view.capital > 0 {
        invoke_signed(
            &perc::withdraw(
                &vault_key,
                market.key,
                portfolio.key,
                buffer.key,
                perc_vault.key,
                perc_vault_auth.key,
                view.portfolio_id,
                view.sequence,
                view.capital,
            ),
            &[
                vault_ai.clone(),
                market.clone(),
                portfolio.clone(),
                buffer.clone(),
                perc_vault.clone(),
                perc_vault_auth.clone(),
                token.clone(),
                percolator.clone(),
            ],
            &[signer_seeds],
        )?;
    }
    let buffer_after = token_amount(buffer)?;
    if buffer_after < buffer_before {
        return Err(VaultError::Overflow.into());
    }
    let after = perc::read_portfolio(portfolio, market.key, &vault_key)?;

    // 3. Net asset value. Low: collateral actually held, minus what is owed or queued.
    //    High: plus profit the engine has booked but not yet released as capital. Withdrawals
    //    are priced low and deposits high, so neither side can extract value from the other.
    //    With the position open, capital still in Percolator counts too, less any loss the
    //    engine has not taken out of it yet.
    let cash = sub(sub(buffer_after, v.reserved_assets)?, v.pending_deposit_assets)?;
    let deployed = if open {
        let capital = u64::try_from(after.capital).map_err(|_| VaultError::Overflow)?;
        let loss = u64::try_from((-after.pnl).max(0)).unwrap_or(u64::MAX);
        capital.saturating_sub(loss)
    } else {
        0
    };
    let nav_low = add(cash, deployed)?;
    let unrealized = if after.pnl > 0 {
        u64::try_from(after.pnl).unwrap_or(u64::MAX)
    } else {
        0
    };
    let supply = mint_supply(share_mint)?;
    let w = v.pending_withdraw_shares;
    let assets_out = math::assets_for_shares(w, nav_low, supply)?;
    let supply_after_burn = sub(supply, w)?;
    let nav_after_out_low = sub(nav_low, assets_out)?;
    // Fees sitting in the vault's asset insurance above the floor belong to current holders too;
    // counting them in the deposit price stops a deposit from buying into them at a discount.
    // Only the depositors' part: the opener's cut of those fees will not belong to the pool.
    let pending_fees = if v.mode == MODE_OPERATE {
        let excess = insurance_excess(market, &v)?;
        excess - opener_cut(&v, excess)
    } else {
        0
    };
    let nav_high = nav_after_out_low
        .saturating_add(unrealized)
        .saturating_add(pending_fees);
    let d = v.pending_deposit_assets;
    let (minted, refund) = if d == 0 {
        (0, false)
    } else if nav_high == 0 && supply_after_burn > 0 {
        // Shares exist but back nothing: pricing a deposit would hand the old holders' zero-value
        // shares a claim on it or overflow. Refund the epoch's deposits instead.
        (0, true)
    } else {
        match math::shares_for_assets(d, nav_high, supply_after_burn) {
            Some(0) | None => (0, true),
            Some(m) => (m, false),
        }
    };

    // 4. Record the epoch. The record is created here, so it cannot be forged or replayed.
    let (record_key, record_bump) = epoch_address(program_id, &vault_key, v.epoch);
    key_is(record_ai, &record_key)?;
    let epoch_bytes = v.epoch.to_le_bytes();
    create_pda_account(
        cranker,
        record_ai,
        system,
        program_id,
        EPOCH_RECORD_LEN,
        &[SEED_EPOCH, vault_key.as_ref(), &epoch_bytes, &[record_bump]],
    )?;
    state::store_epoch(
        record_ai,
        &EpochRecord {
            magic: EPOCH_MAGIC,
            vault: vault_key,
            epoch: v.epoch,
            deposit_assets: d,
            shares_minted: minted,
            withdraw_shares: w,
            assets_out,
            nav_low,
            nav_high,
            supply_before: supply,
            rolled_slot: now,
            deposits_refunded: refund as u8,
            bump: record_bump,
            _pad: [0; 6],
        },
    )?;

    // 5. Burn the withdrawn shares, mint the new ones into escrow for their owners to claim.
    if w > 0 {
        invoke_signed(
            &spl_token::instruction::burn(&spl_token::ID, escrow.key, share_mint.key, &vault_key, &[], w)?,
            &[escrow.clone(), share_mint.clone(), vault_ai.clone(), token.clone()],
            &[signer_seeds],
        )?;
    }
    if minted > 0 {
        invoke_signed(
            &spl_token::instruction::mint_to(&spl_token::ID, share_mint.key, escrow.key, &vault_key, &[], minted)?,
            &[share_mint.clone(), escrow.clone(), vault_ai.clone(), token.clone()],
            &[signer_seeds],
        )?;
    }
    let refunded = if refund { d } else { 0 };
    v.reserved_assets = add(add(v.reserved_assets, assets_out)?, refunded)?;
    v.pending_deposit_assets = 0;
    v.pending_withdraw_shares = 0;
    // Everything owed must be in the buffer. Flat, it always is (all capital was pulled back).
    // Open, the withdrawals have to fit in the cash: if they do not, this epoch can only settle
    // flat (`RequireFlat`).
    if buffer_after < v.reserved_assets {
        return Err(VaultError::NotFlat.into());
    }

    // 6. Redeploy what is not owed into the LP portfolio, keeping the cash reserve.
    let nav_after = add(nav_after_out_low, if refund { 0 } else { d })?;
    let reserve = (nav_after as u128 * RESERVE_BPS as u128 / 10_000) as u64;
    let deployable = sub(buffer_after, v.reserved_assets)?.saturating_sub(reserve);
    if deployable > 0 {
        let fresh = perc::read_portfolio(portfolio, market.key, &vault_key)?;
        invoke_signed(
            &perc::deposit(
                &vault_key,
                market.key,
                portfolio.key,
                buffer.key,
                perc_vault.key,
                fresh.portfolio_id,
                fresh.sequence,
                deployable as u128,
            ),
            &[
                vault_ai.clone(),
                market.clone(),
                portfolio.clone(),
                buffer.clone(),
                perc_vault.clone(),
                token.clone(),
                percolator.clone(),
            ],
            &[signer_seeds],
        )?;
    }
    v.last_nav = nav_after.saturating_add(unrealized);
    if !open {
        v.inventory = 0; // proven flat above
    }
    v.needs_flat = 0;
    v.epoch = add(v.epoch, 1)?;
    v.epoch_start_slot = now;
    state::store_vault(vault_ai, &v)?;

    // 7. Renew the matcher approval for another TTL.
    let fresh = perc::read_portfolio(portfolio, market.key, &vault_key)?;
    let expiry = now.checked_add(v.matcher_ttl_slots).ok_or(VaultError::Overflow)?;
    invoke_signed(
        &perc::set_matcher_config(
            &vault_key,
            market.key,
            portfolio.key,
            program_id,
            &vault_key,
            &v.matcher_delegate,
            &fresh,
            frontier,
            v.trade_fee_cap_bps,
            expiry,
        ),
        &[
            vault_ai.clone(),
            market.clone(),
            portfolio.clone(),
            this_program.clone(),
            delegate.clone(),
            percolator.clone(),
        ],
        &[signer_seeds],
    )
}

/// Pays out a ticket from a settled epoch (or, in a terminal vault, refunds an unsettled one).
/// Accounts: 0 user [s], 1 vault [w], 2 ticket [w], 3 epoch record, 4 user collateral [w],
/// 5 user shares [w], 6 buffer [w], 7 share escrow [w], 8 token program.
fn claim(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let user = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let ticket = acc(accounts, 2)?;
    let record_ai = acc(accounts, 3)?;
    let user_collateral = acc(accounts, 4)?;
    let user_shares = acc(accounts, 5)?;
    let buffer = acc(accounts, 6)?;
    let escrow = acc(accounts, 7)?;
    let token = acc(accounts, 8)?;
    signer(user)?;
    for w in [vault_ai, ticket, user_collateral, user_shares, buffer, escrow] {
        writable(w)?;
    }
    key_is(token, &spl_token::ID)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    let vault_key = *vault_ai.key;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, &vault_key)?;
    check_vault_token_account(escrow, &v.share_escrow, &v.share_mint, &vault_key)?;
    let (ticket_key, _) = ticket_address(program_id, &vault_key, user.key);
    key_is(ticket, &ticket_key)?;
    let mut t = state::load_ticket(ticket, program_id)?;
    if t.vault != vault_key || t.owner != *user.key {
        return Err(VaultError::BadAccount.into());
    }
    if t.deposit_assets == 0 && t.withdraw_shares == 0 {
        return Err(VaultError::NothingToClaim.into());
    }

    let (assets_out, shares_out) = if t.epoch < v.epoch {
        let (record_key, _) = epoch_address(program_id, &vault_key, t.epoch);
        key_is(record_ai, &record_key)?;
        let r = state::load_epoch(record_ai, program_id)?;
        if r.vault != vault_key || r.epoch != t.epoch {
            return Err(VaultError::BadAccount.into());
        }
        let mut assets = math::pro_rata(t.withdraw_shares, r.assets_out, r.withdraw_shares)?;
        let mut shares = 0;
        if r.deposits_refunded != 0 {
            assets = add(assets, t.deposit_assets)?;
        } else {
            shares = math::pro_rata(t.deposit_assets, r.shares_minted, r.deposit_assets)?;
        }
        // Everything paid in collateral here was reserved at the roll.
        v.reserved_assets = sub(v.reserved_assets, assets)?;
        (assets, shares)
    } else if v.status == STATUS_TERMINAL && t.epoch == v.epoch {
        // The epoch will never settle: return the deposit and the escrowed shares as they were.
        v.pending_deposit_assets = sub(v.pending_deposit_assets, t.deposit_assets)?;
        v.pending_withdraw_shares = sub(v.pending_withdraw_shares, t.withdraw_shares)?;
        (t.deposit_assets, t.withdraw_shares)
    } else {
        return Err(VaultError::EpochNotRolled.into());
    };

    t.deposit_assets = 0;
    t.withdraw_shares = 0;
    t.epoch = v.epoch;
    state::store_ticket(ticket, &t)?;
    state::store_vault(vault_ai, &v)?;

    let bump = v.vault_bump;
    let signer_seeds: &[&[u8]] = vault_seeds!(v, bump);
    if assets_out > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(&spl_token::ID, buffer.key, user_collateral.key, &vault_key, &[], assets_out)?,
            &[buffer.clone(), user_collateral.clone(), vault_ai.clone(), token.clone()],
            &[signer_seeds],
        )?;
    }
    if shares_out > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(&spl_token::ID, escrow.key, user_shares.key, &vault_key, &[], shares_out)?,
            &[escrow.clone(), user_shares.clone(), vault_ai.clone(), token.clone()],
            &[signer_seeds],
        )?;
    }
    Ok(())
}

/// Renews the vault's matcher approval on its LP portfolio. Anyone can call it; it only ever
/// re-approves the vault's own program, context and delegate, with the vault's own fee cap.
/// Accounts: 0 vault, 1 market, 2 LP portfolio [w], 3 this program, 4 matcher delegate,
/// 5 Percolator program. Data: the market's current asset-generation frontier.
fn refresh_matcher(program_id: &Pubkey, accounts: &[AccountInfo], frontier: u64) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let this_program = acc(accounts, 3)?;
    let delegate = acc(accounts, 4)?;
    let percolator = acc(accounts, 5)?;
    writable(portfolio)?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    key_is(this_program, program_id)?;
    let v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    key_is(delegate, &v.matcher_delegate)?;
    let view = perc::read_portfolio(portfolio, market.key, vault_ai.key)?;
    let now = Clock::get()?.slot;
    let expiry = now.checked_add(v.matcher_ttl_slots).ok_or(VaultError::Overflow)?;
    let bump = v.vault_bump;
    invoke_signed(
        &perc::set_matcher_config(
            vault_ai.key,
            market.key,
            portfolio.key,
            program_id,
            vault_ai.key,
            &v.matcher_delegate,
            &view,
            frontier,
            v.trade_fee_cap_bps,
            expiry,
        ),
        &[
            vault_ai.clone(),
            market.clone(),
            portfolio.clone(),
            this_program.clone(),
            delegate.clone(),
            percolator.clone(),
        ],
        &[vault_seeds!(v, bump)],
    )
}

/// Converts the LP portfolio's released profit into capital, so the next roll counts it as
/// withdrawable value. Accounts: 0 vault, 1 market [w], 2 LP portfolio [w], 3 Percolator program.
fn convert_pnl(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let percolator = acc(accounts, 3)?;
    writable(market)?;
    writable(portfolio)?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let v = state::load_vault(vault_ai, program_id)?;
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    let view = perc::read_portfolio(portfolio, market.key, vault_ai.key)?;
    let bump = v.vault_bump;
    invoke_signed(
        &perc::convert_released_pnl(
            vault_ai.key,
            market.key,
            portfolio.key,
            view.portfolio_id,
            view.position_epoch,
        ),
        &[vault_ai.clone(), market.clone(), portfolio.clone(), percolator.clone()],
        &[vault_seeds!(v, bump)],
    )
}

/// After the market is resolved: closes the LP portfolio through Percolator's resolved path and
/// moves the payout into the buffer. May need several calls; when the portfolio is empty the
/// vault becomes terminal and shares redeem pro rata.
///
/// Accounts: 0 vault [w], 1 market [w], 2 LP portfolio [w], 3 buffer [w],
/// 4 Percolator collateral vault [w], 5 Percolator vault authority, 6 token program,
/// 7 Percolator program.
fn settle_resolved(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let buffer = acc(accounts, 3)?;
    let perc_vault = acc(accounts, 4)?;
    let perc_vault_auth = acc(accounts, 5)?;
    let token = acc(accounts, 6)?;
    let percolator = acc(accounts, 7)?;
    for w in [vault_ai, market, portfolio, buffer, perc_vault] {
        writable(w)?;
    }
    programs(percolator, token)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    key_is(perc_vault_auth, &perc::vault_authority(&v.market))?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    let bump = v.vault_bump;
    invoke_signed(
        &perc::close_resolved(
            vault_ai.key,
            market.key,
            portfolio.key,
            buffer.key,
            perc_vault.key,
            perc_vault_auth.key,
        ),
        &[
            vault_ai.clone(),
            market.clone(),
            portfolio.clone(),
            buffer.clone(),
            perc_vault.clone(),
            perc_vault_auth.clone(),
            token.clone(),
            percolator.clone(),
        ],
        &[vault_seeds!(v, bump)],
    )?;
    // Terminal once nothing is left in the portfolio: no position, no capital, no profit.
    let view = perc::read_portfolio(portfolio, market.key, vault_ai.key)?;
    if view.flat && view.capital == 0 && view.pnl == 0 {
        v.status = STATUS_TERMINAL;
        v.inventory = 0;
        state::store_vault(vault_ai, &v)?;
    }
    Ok(())
}

/// In a terminal vault, burns shares for their pro-rata part of the free collateral.
/// Accounts: 0 user [s], 1 vault [w], 2 user shares [w], 3 share mint [w],
/// 4 user collateral [w], 5 buffer [w], 6 token program.
fn redeem_terminal(program_id: &Pubkey, accounts: &[AccountInfo], shares: u64) -> ProgramResult {
    if shares == 0 {
        return Err(VaultError::ZeroAmount.into());
    }
    let user = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let user_shares = acc(accounts, 2)?;
    let share_mint = acc(accounts, 3)?;
    let user_collateral = acc(accounts, 4)?;
    let buffer = acc(accounts, 5)?;
    let token = acc(accounts, 6)?;
    signer(user)?;
    for w in [vault_ai, user_shares, share_mint, user_collateral, buffer] {
        writable(w)?;
    }
    key_is(token, &spl_token::ID)?;
    let v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_TERMINAL {
        return Err(VaultError::NotTerminal.into());
    }
    key_is(share_mint, &v.share_mint)?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    let free = sub(sub(token_amount(buffer)?, v.reserved_assets)?, v.pending_deposit_assets)?;
    let supply = mint_supply(share_mint)?;
    // Plain pro rata (no virtual offset): the whole remaining value belongs to the holders.
    let out = math::pro_rata(shares, free, supply)?;
    let bump = v.vault_bump;
    let vault_key = *vault_ai.key;
    invoke(
        &spl_token::instruction::burn(&spl_token::ID, user_shares.key, share_mint.key, user.key, &[], shares)?,
        &[user_shares.clone(), share_mint.clone(), user.clone(), token.clone()],
    )?;
    if out > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(&spl_token::ID, buffer.key, user_collateral.key, &vault_key, &[], out)?,
            &[buffer.clone(), user_collateral.clone(), vault_ai.clone(), token.clone()],
            &[vault_seeds!(v, bump)],
        )?;
    }
    Ok(())
}

/// Moves collateral from any other token account the vault owns into the buffer (for example a
/// resolved payout someone routed to a different vault-owned account).
/// Accounts: 0 vault, 1 stray token account [w], 2 buffer [w], 3 token program.
fn sweep(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let stray = acc(accounts, 1)?;
    let buffer = acc(accounts, 2)?;
    let token = acc(accounts, 3)?;
    writable(stray)?;
    writable(buffer)?;
    key_is(token, &spl_token::ID)?;
    let v = state::load_vault(vault_ai, program_id)?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    if stray.key == buffer.key {
        return Err(VaultError::BadAccount.into());
    }
    let s = token_account(stray)?;
    if s.mint != v.collateral_mint || s.owner != *vault_ai.key || s.amount == 0 {
        return Err(VaultError::BadAccount.into());
    }
    let bump = v.vault_bump;
    invoke_signed(
        &spl_token::instruction::transfer(&spl_token::ID, stray.key, buffer.key, vault_ai.key, &[], s.amount)?,
        &[stray.clone(), buffer.clone(), vault_ai.clone(), token.clone()],
        &[vault_seeds!(v, bump)],
    )
}

/// Insurance of the vault's asset above its floor: what harvesting may move to depositors.
fn insurance_excess(market: &AccountInfo, v: &VaultState) -> Result<u64, VaultError> {
    let (id, remaining) =
        perc::asset_insurance(market, v.asset_index).map_err(|_| VaultError::BadPercolatorAccount)?;
    if id != v.asset_market_id {
        return Err(VaultError::UnknownAsset);
    }
    let excess = remaining.saturating_sub(v.insurance_floor as u128);
    Ok(u64::try_from(excess).unwrap_or(u64::MAX))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListArgs {
    pub asset_index: u16,
    pub market_id: u64,
    pub activation_authority_epoch: u64,
    pub initial_price: u64,
    pub oracle_observation_sequence: u64,
    pub oracle_authority_epoch: u64,
}

impl ListArgs {
    pub const LEN: usize = 2 + 8 * 5;
    pub fn encode(&self) -> Vec<u8> {
        let mut d = vec![TAG_LIST_ASSET];
        d.extend_from_slice(&self.asset_index.to_le_bytes());
        d.extend_from_slice(&self.market_id.to_le_bytes());
        d.extend_from_slice(&self.activation_authority_epoch.to_le_bytes());
        d.extend_from_slice(&self.initial_price.to_le_bytes());
        d.extend_from_slice(&self.oracle_observation_sequence.to_le_bytes());
        d.extend_from_slice(&self.oracle_authority_epoch.to_le_bytes());
        d
    }
    pub fn decode(d: &[u8]) -> Result<Self, VaultError> {
        if d.len() != Self::LEN {
            return Err(VaultError::InvalidInstruction);
        }
        let mut r = Reader(d);
        Ok(Self {
            asset_index: r.u16(),
            market_id: r.u64(),
            activation_authority_epoch: r.u64(),
            initial_price: r.u64(),
            oracle_observation_sequence: r.u64(),
            oracle_authority_epoch: r.u64(),
        })
    }
}

/// Operate mode: lists the vault's own asset on the market and configures its price feed from the
/// parameters fixed at creation. The vault PDA signs the activation, so it becomes the asset's
/// admin, and it names itself as all four domain authorities: no person holds any key over the
/// asset. Anyone can call this; the caller pays Percolator's listing fee (any unused part of
/// `listing_fee_max` is refunded) and the market account's extra rent. Every argument is checked
/// by Percolator against the market's current state.
///
/// Accounts: 0 payer [s,w], 1 payer collateral [w], 2 vault [w], 3 market [w], 4 buffer [w],
/// 5 Percolator collateral vault [w], 6 token program, 7 system program, 8 Percolator program,
/// 9.. the oracle price accounts, one per leg.
fn list_asset(program_id: &Pubkey, accounts: &[AccountInfo], a: ListArgs) -> ProgramResult {
    let payer = acc(accounts, 0)?;
    let payer_collateral = acc(accounts, 1)?;
    let vault_ai = acc(accounts, 2)?;
    let market = acc(accounts, 3)?;
    let buffer = acc(accounts, 4)?;
    let perc_vault = acc(accounts, 5)?;
    let token = acc(accounts, 6)?;
    let system = acc(accounts, 7)?;
    let percolator = acc(accounts, 8)?;
    signer(payer)?;
    for w in [payer, payer_collateral, vault_ai, market, buffer, perc_vault] {
        writable(w)?;
    }
    programs(percolator, token)?;
    key_is(system, &system_program::ID)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.mode != MODE_OPERATE {
        return Err(VaultError::WrongMode.into());
    }
    if v.status != STATUS_PENDING_LISTING {
        return Err(VaultError::AlreadyInitialized.into());
    }
    key_is(market, &v.market)?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    if a.asset_index == u16::MAX {
        return Err(VaultError::InvalidParams.into());
    }
    // One Pyth account: a fully verified, fresh update for the vault's feed sets the listing price.
    if accounts.len() != 10 || v.oracle_leg_count != 1 || v.oracle_invert != 0 || v.oracle_unit_scale != 0 {
        return Err(VaultError::BadAccount.into());
    }
    let clock = Clock::get()?;
    let update = crate::pyth::read(acc(accounts, 9)?, &v.oracle_feeds[0])?;
    if update.publish_time < clock.unix_timestamp.saturating_sub(v.oracle_max_staleness_secs as i64) {
        return Err(VaultError::BadOracle.into());
    }
    let price = update.price_e6().ok_or(VaultError::BadOracle)?;

    // Rent for the market account's new asset slot.
    let new_len = perc::market_account_len_for_slots(a.asset_index as usize + 1);
    if new_len > market.data_len() {
        let need = Rent::get()?.minimum_balance(new_len);
        if market.lamports() < need {
            invoke(
                &system_instruction::transfer(payer.key, market.key, need - market.lamports()),
                &[payer.clone(), market.clone(), system.clone()],
            )?;
        }
    }

    // The listing fee is paid from a vault-owned account (Percolator requires the fee source to
    // belong to the activating signer): stage it in the buffer, measure what was taken, refund
    // the rest. Pending deposits in the buffer are untouched because only the delta moves.
    let fee_max = v.listing_fee_max;
    invoke(
        &spl_token::instruction::transfer(&spl_token::ID, payer_collateral.key, buffer.key, payer.key, &[], fee_max)?,
        &[payer_collateral.clone(), buffer.clone(), payer.clone(), token.clone()],
    )?;
    let before = token_amount(buffer)?;
    let bump = v.vault_bump;
    let vault_key = *vault_ai.key;
    invoke_signed(
        &perc::activate_asset(
            &vault_key,
            market.key,
            buffer.key,
            perc_vault.key,
            a.asset_index,
            a.market_id,
            a.activation_authority_epoch,
            clock.slot,
            price,
            fee_max as u128,
            &vault_key,
        ),
        &[vault_ai.clone(), market.clone(), buffer.clone(), perc_vault.clone(), token.clone(), percolator.clone()],
        &[vault_seeds!(v, bump)],
    )?;
    let spent = sub(before, token_amount(buffer)?)?;
    let refund = sub(fee_max, spent)?;
    if refund > 0 {
        invoke_signed(
            &spl_token::instruction::transfer(&spl_token::ID, buffer.key, payer_collateral.key, &vault_key, &[], refund)?,
            &[buffer.clone(), payer_collateral.clone(), vault_ai.clone(), token.clone()],
            &[vault_seeds!(v, bump)],
        )?;
    }

    // The asset's mark is whatever its oracle authority (this vault) pushes, and the vault only
    // pushes on the router's instruction, with a verified Pyth price. It starts at the listing price.
    invoke_signed(
        &perc::auth_mark(
            false,
            &vault_key,
            market.key,
            a.asset_index,
            a.market_id,
            clock.slot,
            price,
            a.oracle_observation_sequence,
            a.oracle_authority_epoch,
        ),
        &[vault_ai.clone(), market.clone(), percolator.clone()],
        &[vault_seeds!(v, bump)],
    )?;

    // Record the listed asset only if the slot really holds the generation we activated.
    let (id, _) = perc::asset_insurance(market, a.asset_index)?;
    if id != a.market_id {
        return Err(VaultError::UnknownAsset.into());
    }
    v.asset_index = a.asset_index;
    v.asset_market_id = a.market_id;
    v.status = STATUS_ACTIVE;
    v.epoch_start_slot = clock.slot;
    v.listed_slot = clock.slot;
    state::store_vault(vault_ai, &v)
}

/// Checks that `ai` is the router's authority and signed.
fn router_signed(ai: &AccountInfo) -> Result<(), VaultError> {
    if !ai.is_signer || *ai.key != crate::router_authority().0 {
        return Err(VaultError::NotRouter);
    }
    Ok(())
}

/// Moves the vault's asset mark. Only the router calls it, and only with a verified Pyth price:
/// the latest one, or the first one published at a queued trade's target time. The vault is the
/// asset's oracle authority, so Percolator takes the mark as given (and moves its effective
/// price toward it at most its per-slot cap).
///
/// Accounts: 0 router authority [signer], 1 vault, 2 market [w], 3 Percolator program.
/// Data: mark (e6), the oracle observation sequence and the asset's authority epoch.
fn push_mark(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    mark_e6: u64,
    observation_sequence: u64,
    authority_epoch: u64,
) -> ProgramResult {
    let router = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let market = acc(accounts, 2)?;
    let percolator = acc(accounts, 3)?;
    router_signed(router)?;
    writable(market)?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let v = state::load_vault(vault_ai, program_id)?;
    if v.mode != MODE_OPERATE || v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    let bump = v.vault_bump;
    invoke_signed(
        &perc::auth_mark(
            true,
            vault_ai.key,
            market.key,
            v.asset_index,
            v.asset_market_id,
            Clock::get()?.slot,
            mark_e6,
            observation_sequence,
            authority_epoch,
        ),
        &[vault_ai.clone(), market.clone(), percolator.clone()],
        &[vault_seeds!(v, bump)],
    )
}

/// Arms the one fill the matcher will accept in this slot: the router calls it right before the
/// trade it executes. Every other fill (a taker trading directly) is refused by the matcher.
///
/// Accounts: 0 router authority [signer], 1 vault [w]. Data: taker size (i128, + = taker buys).
fn arm_fill(program_id: &Pubkey, accounts: &[AccountInfo], size: i128) -> ProgramResult {
    let router = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    router_signed(router)?;
    writable(vault_ai)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.mode != MODE_OPERATE || v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    if size == 0 || size == i128::MIN {
        return Err(VaultError::InvalidParams.into());
    }
    v.armed_size = size;
    v.armed_slot = Clock::get()?.slot;
    state::store_vault(vault_ai, &v)
}

/// Switches the vault to closing-only for the rest of an overdue epoch. Allowed when the epoch is
/// over and its withdrawals, valued at the vault's current net asset value, exceed the cash the
/// vault holds outside Percolator: they can then only be paid once the vault's position is
/// closed and its capital pulled back. Anyone can call it; the flag is cleared by the roll.
///
/// Accounts: 0 vault [w], 1 market, 2 LP portfolio, 3 buffer, 4 share mint.
fn require_flat(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let buffer = acc(accounts, 3)?;
    let share_mint = acc(accounts, 4)?;
    writable(vault_ai)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    key_is(share_mint, &v.share_mint)?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    if Clock::get()?.slot < v.epoch_start_slot.saturating_add(v.epoch_len_slots) {
        return Err(VaultError::EpochNotOver.into());
    }
    let view = perc::read_portfolio(portfolio, market.key, vault_ai.key)?;
    let cash = sub(sub(token_amount(buffer)?, v.reserved_assets)?, v.pending_deposit_assets)?;
    let capital = u64::try_from(view.capital).map_err(|_| VaultError::Overflow)?;
    let loss = u64::try_from((-view.pnl).max(0)).unwrap_or(u64::MAX);
    let nav_low = add(cash, capital.saturating_sub(loss))?;
    let owed = math::assets_for_shares(v.pending_withdraw_shares, nav_low, mint_supply(share_mint)?)?;
    // The epoch's own deposits are cash too once it settles.
    if view.flat || owed <= add(cash, v.pending_deposit_assets)? {
        return Err(VaultError::CanSettleOpen.into());
    }
    v.needs_flat = 1;
    state::store_vault(vault_ai, &v)
}

/// Sets the vault's tracked position to what its portfolio actually holds. The matcher tracks
/// the position fill by fill, but Percolator can also change it outside a fill: when a trader
/// on the other side is liquidated or closed out unilaterally, the vault's position is
/// deleveraged with it. Anyone can call this; it only copies Percolator's own figure.
///
/// Accounts: 0 vault [w], 1 market, 2 LP portfolio.
fn sync_inventory(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    writable(vault_ai)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    perc::expect_market(market)?;
    perc::read_portfolio(portfolio, market.key, vault_ai.key)?;
    let held = perc::positions(&market.try_borrow_data()?, &portfolio.try_borrow_data()?)?
        .iter()
        .find(|p| p.asset == v.asset_index)
        .map_or(0, |p| p.size);
    v.inventory = held;
    state::store_vault(vault_ai, &v)
}

/// Makes the program's governor PDA the market's `marketauth`. Percolator requires the current
/// authority to sign and the new one to co-sign; the PDA co-signs here. Afterwards the market has
/// no admin key: the program only ever uses the governor to retire idle markets.
///
/// Accounts: 0 current marketauth [signer], 1 governor PDA, 2 market [w], 3 Percolator program.
/// Data: the market's authority epoch (checked by Percolator).
fn accept_governance(program_id: &Pubkey, accounts: &[AccountInfo], authority_epoch: u64) -> ProgramResult {
    let current = acc(accounts, 0)?;
    let governor = acc(accounts, 1)?;
    let market = acc(accounts, 2)?;
    let percolator = acc(accounts, 3)?;
    signer(current)?;
    writable(market)?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let (expected, bump) = state::governor_address(program_id, market.key);
    key_is(governor, &expected)?;
    invoke_signed(
        &perc::update_market_authority(current.key, governor.key, market.key, authority_epoch),
        &[current.clone(), governor.clone(), market.clone(), percolator.clone()],
        &[&[SEED_GOVERNOR, market.key.as_ref(), &[bump]]],
    )
}

/// Frees the market slot of an idle operate-mode vault, so a market can be opened in it again.
/// Anyone can call it once the vault has no liquidity providers (no shares, no requests), holds
/// no position, and was listed at least `RETIRE_IDLE_EPOCHS` epochs ago; Percolator additionally
/// refuses unless the asset itself is empty. The asset's remaining insurance (trading fees that
/// no depositor owns) is paid to the opener. The vault returns to pending listing, so the same
/// feed can be opened again later, in any free slot.
///
/// Accounts: 0 vault [w], 1 market [w], 2 governor PDA, 3 share mint, 4 LP portfolio,
/// 5 buffer [w], 6 Percolator collateral vault [w], 7 Percolator vault authority,
/// 8 token program, 9 Percolator program, 10 the vault's router book (no requests may be queued).
/// Data: the asset's authority epoch (for the insurance withdrawal) and the market's authority
/// epoch (for the retirement), both checked by Percolator.
fn retire_market(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    asset_authority_epoch: u64,
    market_authority_epoch: u64,
) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let governor = acc(accounts, 2)?;
    let share_mint = acc(accounts, 3)?;
    let portfolio = acc(accounts, 4)?;
    let buffer = acc(accounts, 5)?;
    let perc_vault = acc(accounts, 6)?;
    let perc_vault_auth = acc(accounts, 7)?;
    let token = acc(accounts, 8)?;
    let percolator = acc(accounts, 9)?;
    let book = acc(accounts, 10)?;
    for w in [vault_ai, market, buffer, perc_vault] {
        writable(w)?;
    }
    programs(percolator, token)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.mode != MODE_OPERATE {
        return Err(VaultError::WrongMode.into());
    }
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(share_mint, &v.share_mint)?;
    // A queued trade must be able to fill (or expire) against the market it was made on.
    key_is(book, &crate::router_book(vault_ai.key))?;
    if book.owner == &crate::ROUTER_PROGRAM_ID {
        let d = book.try_borrow_data()?;
        if d.get(crate::ROUTER_BOOK_LEN_OFF).copied().unwrap_or(1) != 0 {
            return Err(VaultError::NotIdle.into());
        }
    }
    key_is(portfolio, &v.lp_portfolio)?;
    key_is(perc_vault_auth, &perc::vault_authority(&v.market))?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    let (expected_governor, governor_bump) = state::governor_address(program_id, market.key);
    key_is(governor, &expected_governor)?;

    let now = Clock::get()?.slot;
    let idle_since = v
        .listed_slot
        .saturating_add(v.epoch_len_slots.saturating_mul(RETIRE_IDLE_EPOCHS));
    if mint_supply(share_mint)? != 0
        || v.pending_deposit_assets != 0
        || v.pending_withdraw_shares != 0
        || now < idle_since
        || !perc::read_portfolio(portfolio, market.key, vault_ai.key)?.flat
    {
        return Err(VaultError::NotIdle.into());
    }

    // Insurance left on the asset must leave before the slot can be reused. With no depositors
    // it belongs to nobody else, so it goes to the opener like the opener's fee share.
    let (id, remaining) = perc::asset_insurance(market, v.asset_index)?;
    if id != v.asset_market_id {
        return Err(VaultError::UnknownAsset.into());
    }
    let bump = v.vault_bump;
    if remaining != 0 {
        let before = token_amount(buffer)?;
        invoke_signed(
            &perc::withdraw_insurance_asset(
                vault_ai.key,
                market.key,
                buffer.key,
                perc_vault.key,
                perc_vault_auth.key,
                v.asset_index,
                v.asset_market_id,
                asset_authority_epoch,
                remaining,
            ),
            &[
                vault_ai.clone(),
                market.clone(),
                buffer.clone(),
                perc_vault.clone(),
                perc_vault_auth.clone(),
                token.clone(),
                percolator.clone(),
            ],
            &[vault_seeds!(v, bump)],
        )?;
        let received = sub(token_amount(buffer)?, before)?;
        v.total_fees_harvested = v.total_fees_harvested.saturating_add(received);
        v.opener_fees_owed = add(v.opener_fees_owed, received)?;
        v.opener_fees_total = v.opener_fees_total.saturating_add(received);
        v.reserved_assets = add(v.reserved_assets, received)?;
    }

    invoke_signed(
        &perc::retire_asset(
            governor.key,
            market.key,
            v.asset_index,
            v.asset_market_id,
            market_authority_epoch,
            now,
        ),
        &[governor.clone(), market.clone(), percolator.clone()],
        &[&[SEED_GOVERNOR, market.key.as_ref(), &[governor_bump]]],
    )?;

    v.status = STATUS_PENDING_LISTING;
    v.asset_index = u16::MAX;
    v.asset_market_id = 0;
    v.inventory = 0;
    v.listed_slot = 0;
    v.epoch_start_slot = now;
    state::store_vault(vault_ai, &v)
}

/// Operate mode: moves trading-fee income from the vault's asset insurance into the buffer,
/// never below `insurance_floor`. Percolator additionally refuses live withdrawals unless the
/// asset is healthy (no lag, stress, lock or deficit). In a terminal vault (market resolved and
/// empty) the floor no longer applies: nothing is left for it to protect.
///
/// Accounts: 0 vault [w], 1 market [w], 2 buffer [w], 3 Percolator collateral vault [w],
/// 4 Percolator vault authority, 5 token program, 6 Percolator program.
/// Data: the asset's authority epoch (checked by Percolator), the most to move.
fn harvest_fees(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    authority_epoch: u64,
    max_amount: u64,
) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let buffer = acc(accounts, 2)?;
    let perc_vault = acc(accounts, 3)?;
    let perc_vault_auth = acc(accounts, 4)?;
    let token = acc(accounts, 5)?;
    let percolator = acc(accounts, 6)?;
    for w in [vault_ai, market, buffer, perc_vault] {
        writable(w)?;
    }
    programs(percolator, token)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.mode != MODE_OPERATE {
        return Err(VaultError::WrongMode.into());
    }
    if v.status != STATUS_ACTIVE && v.status != STATUS_TERMINAL {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(perc_vault_auth, &perc::vault_authority(&v.market))?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    let available = if v.status == STATUS_TERMINAL {
        let (id, remaining) = perc::asset_insurance(market, v.asset_index)?;
        if id != v.asset_market_id {
            return Err(VaultError::UnknownAsset.into());
        }
        u64::try_from(remaining).unwrap_or(u64::MAX)
    } else {
        insurance_excess(market, &v)?
    };
    let amount = core::cmp::min(available, max_amount);
    if amount == 0 {
        return Err(VaultError::NothingToHarvest.into());
    }
    let before = token_amount(buffer)?;
    let bump = v.vault_bump;
    invoke_signed(
        &perc::withdraw_insurance_asset(
            vault_ai.key,
            market.key,
            buffer.key,
            perc_vault.key,
            perc_vault_auth.key,
            v.asset_index,
            v.asset_market_id,
            authority_epoch,
            amount as u128,
        ),
        &[
            vault_ai.clone(),
            market.clone(),
            buffer.clone(),
            perc_vault.clone(),
            perc_vault_auth.clone(),
            token.clone(),
            percolator.clone(),
        ],
        &[vault_seeds!(v, bump)],
    )?;
    let received = sub(token_amount(buffer)?, before)?;
    v.total_fees_harvested = v.total_fees_harvested.saturating_add(received);
    // Set the opener's cut aside: it stays in the buffer but is owed, like a settled withdrawal.
    let cut = opener_cut(&v, received);
    v.opener_fees_owed = add(v.opener_fees_owed, cut)?;
    v.opener_fees_total = v.opener_fees_total.saturating_add(cut);
    v.reserved_assets = add(v.reserved_assets, cut)?;
    state::store_vault(vault_ai, &v)
}

/// Liveness backstop. If an epoch has been over for a further full epoch and its withdrawals
/// still need the vault flat (`RequireFlat`; closing-only quoting did not bring it there, for
/// example because a taker holds its position on purpose), anyone can make the vault close its own position through
/// Percolator's unilateral `RebalanceReduce`, at the engine's effective price. A taker holding a
/// position therefore cannot keep withdrawals locked.
///
/// Accounts: 0 vault [w], 1 market [w], 2 LP portfolio [w], 3 Percolator program.
fn unwind(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let vault_ai = acc(accounts, 0)?;
    let market = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let percolator = acc(accounts, 3)?;
    for w in [vault_ai, market, portfolio] {
        writable(w)?;
    }
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.status != STATUS_ACTIVE {
        return Err(VaultError::NotActive.into());
    }
    key_is(market, &v.market)?;
    key_is(portfolio, &v.lp_portfolio)?;
    let now = Clock::get()?.slot;
    let overdue = v
        .epoch_start_slot
        .saturating_add(v.epoch_len_slots.saturating_mul(2));
    // Only when the epoch cannot settle with the position open (`RequireFlat`).
    if now < overdue || v.needs_flat == 0 {
        return Err(VaultError::EpochNotOver.into());
    }
    let view = perc::read_portfolio(portfolio, market.key, vault_ai.key)?;
    if view.flat {
        return Err(VaultError::NothingToClaim.into());
    }
    let bump = v.vault_bump;
    invoke_signed(
        &perc::rebalance_reduce(
            vault_ai.key,
            market.key,
            portfolio.key,
            view.portfolio_id,
            view.position_epoch,
            v.asset_index,
            u128::MAX >> 1,
        ),
        &[vault_ai.clone(), market.clone(), portfolio.clone(), percolator.clone()],
        &[vault_seeds!(v, bump)],
    )?;
    if perc::read_portfolio(portfolio, market.key, vault_ai.key)?.flat {
        v.inventory = 0;
        state::store_vault(vault_ai, &v)?;
    }
    Ok(())
}

/// The opener of a canonical market collects its accrued share of the market's fees.
/// Accounts: 0 opener [s], 1 vault [w], 2 destination collateral account [w], 3 buffer [w],
/// 4 token program.
fn claim_opener_fees(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let opener = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let dest = acc(accounts, 2)?;
    let buffer = acc(accounts, 3)?;
    let token = acc(accounts, 4)?;
    signer(opener)?;
    for w in [vault_ai, dest, buffer] {
        writable(w)?;
    }
    key_is(token, &spl_token::ID)?;
    let mut v = state::load_vault(vault_ai, program_id)?;
    if v.vault_kind != KIND_CANONICAL {
        return Err(VaultError::WrongMode.into());
    }
    key_is(opener, &v.creator)?;
    check_vault_token_account(buffer, &v.buffer, &v.collateral_mint, vault_ai.key)?;
    let amount = v.opener_fees_owed;
    if amount == 0 {
        return Err(VaultError::NothingToClaim.into());
    }
    v.opener_fees_owed = 0;
    v.reserved_assets = sub(v.reserved_assets, amount)?;
    state::store_vault(vault_ai, &v)?;
    let bump = v.vault_bump;
    invoke_signed(
        &spl_token::instruction::transfer(&spl_token::ID, buffer.key, dest.key, vault_ai.key, &[], amount)?,
        &[buffer.clone(), dest.clone(), vault_ai.clone(), token.clone()],
        &[vault_seeds!(v, bump)],
    )
}
