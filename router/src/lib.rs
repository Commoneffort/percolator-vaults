//! Percolator Vaults router: the only way to trade against a vault, and the reason nobody can
//! front-run one.
//!
//! A trade is a request first. The request lands on chain and gets a target time: the later of
//! the chain clock and the vault's current mark time, plus `DELAY_SECS`. It then fills at exactly
//! the first Pyth price published at or after that target, which nobody (the trader, a keeper, a
//! validator, a Pyth publisher) could have known when the request was made:
//!
//! - The price is a fully verified Pyth update, and it is the unique first one at or after the
//!   target (`prev_publish_time < target <= publish_time`), so whoever executes the fill, and
//!   whenever, the price is the same. Nobody gets to pick a price.
//! - The vault's mark only ever moves to verified Pyth prices, forward in time, and never past a
//!   pending request's target before that request is filled or has expired.
//! - The vault's matcher refuses every fill the router did not arm, so there is no other way in.
//! - A request cannot be cancelled. It fills, or it expires after `GRACE_SECS` and its bond is
//!   forfeited, and `Request` checks margin with a stress buffer, so a trader cannot back out of
//!   a fill after seeing its price.
//! - Filling is permissionless: anyone can execute any request, the trader included. Keepers
//!   cannot censor, and the executor earns the request's bond.
//!
//! Each trader's Percolator portfolio and collateral account are owned by a program address
//! derived from their wallet. Only the router can sign for it, and withdrawals only go back to
//! that wallet. There is no admin instruction.

#[cfg(not(target_os = "solana"))]
pub mod client;
pub mod state;

use solana_program::msg;
use bytemuck::Zeroable;
use percolator_vault::{
    percolator::{self as perc, PERCOLATOR_PROGRAM_ID},
    processor::{TAG_ARM_FILL, TAG_PUSH_MARK},
    pyth,
    state::{self as vstate, MODE_OPERATE, STATUS_ACTIVE},
};
use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::Sysvar,
};
use state::*;

solana_program::declare_id!("DkK9TSMpVXLq26HeqxTXLysXyRKDYHTKU94SLFDWgjw3");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

/// Seconds between a request landing and the price it fills at. It must exceed the time for a
/// real-world price move to reach a published Pyth price, plus any lag of the chain clock.
pub const DELAY_SECS: i64 = 4;
/// How long after its target a request can still be filled before anyone may expire it.
pub const GRACE_SECS: i64 = 90;
/// Paid by the trader with each request: to the executor on a fill, forfeited on expiry.
pub const BOND_LAMPORTS: u64 = 2_000_000;
/// Extra price move (bps) the margin check at request time must survive, on every position.
pub const STRESS_BPS: u128 = 1_000;
/// A trader whose equity is below this share (bps) of the notional of their positions can be
/// liquidated through the router: anyone queues it, and at the first Pyth price after its target
/// the position is closed against the vault if the account is still below this level. It sits
/// above Percolator's own maintenance margin on purpose. Percolator liquidates by reducing the
/// position unilaterally, which deleverages the other side and makes the market close-only
/// until every position on it is closed; closing against the vault first avoids that.
pub const LIQUIDATION_BPS: u128 = 1_250;
const POS_SCALE: u128 = 1_000_000;

pub const TAG_OPEN_ACCOUNT: u8 = 0;
pub const TAG_DEPOSIT: u8 = 1;
pub const TAG_WITHDRAW: u8 = 2;
pub const TAG_OPEN_BOOK: u8 = 3;
pub const TAG_REQUEST: u8 = 4;
pub const TAG_ADVANCE: u8 = 5;
pub const TAG_FILL: u8 = 6;
pub const TAG_EXPIRE: u8 = 7;
pub const TAG_REQUEST_CLOSE: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum RouterError {
    InvalidInstruction = 0x5700,
    BadAccount,
    NotSigner,
    NotWritable,
    AlreadyInitialized,
    /// The trader already has a request pending.
    RequestPending,
    /// Margin would not cover the position after a stressed price move.
    InsufficientMargin,
    /// The book already holds the maximum number of pending requests.
    BookFull,
    /// A Pyth update that is not newer than the mark, or not the first one at a pending target.
    WrongUpdate,
    /// The mark is not yet the price this request fills at.
    NotAtTarget,
    /// Percolator's effective price has not converged to the mark yet (it moves a capped amount
    /// per slot); try again in a later slot.
    NotConverged,
    /// The request cannot expire yet.
    TooEarly,
    Overflow,
    NotActive,
    BadOracle,
    /// The market is close-only (a position on it was force-reduced): positions on it can only be
    /// closed, with `RequestClose`, until all are closed and it resets.
    CloseOnly,
    /// `RequestClose` when the market is not close-only and the trader is not liquidatable.
    NotCloseOnly,
}

impl From<RouterError> for ProgramError {
    fn from(e: RouterError) -> Self {
        ProgramError::Custom(e as u32)
    }
}

pub fn process_instruction(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (tag, rest) = data.split_first().ok_or(RouterError::InvalidInstruction)?;
    let u64_at = |i: usize| -> Result<u64, RouterError> {
        rest.get(i..i + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or(RouterError::InvalidInstruction)
    };
    match *tag {
        TAG_OPEN_ACCOUNT => open_account(program_id, accounts),
        TAG_DEPOSIT => deposit(program_id, accounts, u64_at(0)?),
        TAG_WITHDRAW => withdraw(program_id, accounts, u64_at(0)?),
        TAG_OPEN_BOOK => open_book(program_id, accounts),
        TAG_REQUEST => {
            let b = rest.get(..16).ok_or(RouterError::InvalidInstruction)?;
            request(program_id, accounts, i128::from_le_bytes(b.try_into().unwrap()))
        }
        TAG_ADVANCE => advance(program_id, accounts, u64_at(0)?, u64_at(8)?),
        TAG_FILL => fill(program_id, accounts),
        TAG_EXPIRE => expire(program_id, accounts),
        TAG_REQUEST_CLOSE => request_close(program_id, accounts),
        _ => Err(RouterError::InvalidInstruction.into()),
    }
}

// ---- helpers ----

fn acc<'a, 'b>(accounts: &'a [AccountInfo<'b>], i: usize) -> Result<&'a AccountInfo<'b>, RouterError> {
    accounts.get(i).ok_or(RouterError::BadAccount)
}
fn signer(ai: &AccountInfo) -> Result<(), RouterError> {
    if ai.is_signer { Ok(()) } else { Err(RouterError::NotSigner) }
}
fn writable(ai: &AccountInfo) -> Result<(), RouterError> {
    if ai.is_writable { Ok(()) } else { Err(RouterError::NotWritable) }
}
fn key_is(ai: &AccountInfo, k: &Pubkey) -> Result<(), RouterError> {
    if ai.key == k { Ok(()) } else { Err(RouterError::BadAccount) }
}
fn token_amount(ai: &AccountInfo) -> Result<u64, RouterError> {
    if ai.owner != &spl_token::ID {
        return Err(RouterError::BadAccount);
    }
    let d = ai.try_borrow_data().map_err(|_| RouterError::BadAccount)?;
    Ok(spl_token::state::Account::unpack(&d).map_err(|_| RouterError::BadAccount)?.amount)
}

fn create_pda<'a>(
    payer: &AccountInfo<'a>,
    target: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    owner: &Pubkey,
    space: usize,
    extra_lamports: u64,
    seeds: &[&[u8]],
) -> ProgramResult {
    if target.owner != &system_program::ID || target.data_len() != 0 {
        return Err(RouterError::AlreadyInitialized.into());
    }
    let need = Rent::get()?.minimum_balance(space).saturating_add(extra_lamports);
    let have = target.lamports();
    if have < need {
        invoke(
            &system_instruction::transfer(payer.key, target.key, need - have),
            &[payer.clone(), target.clone(), system.clone()],
        )?;
    }
    invoke_signed(&system_instruction::allocate(target.key, space as u64), &[target.clone(), system.clone()], &[seeds])?;
    invoke_signed(&system_instruction::assign(target.key, owner), &[target.clone(), system.clone()], &[seeds])
}

/// Moves all lamports out of a router-owned account (closing it): `bond` to `bond_to`, the rest to `rest_to`.
fn close(account: &AccountInfo, bond: u64, bond_to: &AccountInfo, rest_to: &AccountInfo) -> ProgramResult {
    let total = account.lamports();
    let bond = bond.min(total);
    **bond_to.try_borrow_mut_lamports()? = bond_to.lamports().checked_add(bond).ok_or(RouterError::Overflow)?;
    **rest_to.try_borrow_mut_lamports()? = rest_to.lamports().checked_add(total - bond).ok_or(RouterError::Overflow)?;
    **account.try_borrow_mut_lamports()? = 0;
    account.assign(&system_program::ID);
    account.realloc(0, false)
}

fn load_vault(ai: &AccountInfo) -> Result<vstate::VaultState, ProgramError> {
    let v = vstate::load_vault(ai, &percolator_vault::id())?;
    if v.mode != MODE_OPERATE || v.status != STATUS_ACTIVE {
        return Err(RouterError::NotActive.into());
    }
    Ok(v)
}

fn load_trader(program_id: &Pubkey, ai: &AccountInfo, wallet: &Pubkey) -> Result<TraderState, ProgramError> {
    let t: TraderState = load(ai, program_id, TRADER_MAGIC)?;
    if t.wallet != *wallet {
        return Err(RouterError::BadAccount.into());
    }
    Ok(t)
}

macro_rules! trader_seeds {
    ($t:expr) => {
        &[SEED_TRADER, $t.market.as_ref(), $t.wallet.as_ref(), &[$t.bump]]
    };
}

// ---- accounts ----

/// Creates the trader's router account, its Percolator portfolio and its collateral account,
/// all owned by the trader's program address.
///
/// Accounts: 0 wallet [s, w], 1 trader [w], 2 portfolio [w], 3 collateral [w], 4 market [w],
/// 5 collateral mint, 6 Percolator program, 7 system program, 8 token program.
fn open_account(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let wallet = acc(accounts, 0)?;
    let trader_ai = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let collateral = acc(accounts, 3)?;
    let market = acc(accounts, 4)?;
    let mint = acc(accounts, 5)?;
    let percolator = acc(accounts, 6)?;
    let system = acc(accounts, 7)?;
    let token = acc(accounts, 8)?;
    signer(wallet)?;
    for w in [wallet, trader_ai, portfolio, collateral, market] {
        writable(w)?;
    }
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    key_is(system, &system_program::ID)?;
    key_is(token, &spl_token::ID)?;
    if perc::market_collateral_mint(market)? != *mint.key {
        return Err(RouterError::BadAccount.into());
    }
    let (tk, bump) = trader_address(program_id, market.key, wallet.key);
    let (pk, pbump) = portfolio_address(program_id, market.key, wallet.key);
    let (ck, cbump) = collateral_address(program_id, market.key, wallet.key);
    key_is(trader_ai, &tk)?;
    key_is(portfolio, &pk)?;
    key_is(collateral, &ck)?;

    create_pda(wallet, trader_ai, system, program_id, TRADER_LEN, 0, &[SEED_TRADER, market.key.as_ref(), wallet.key.as_ref(), &[bump]])?;
    create_pda(wallet, portfolio, system, &PERCOLATOR_PROGRAM_ID, perc::PORTFOLIO_ACCOUNT_LEN, 0, &[SEED_PORTFOLIO, market.key.as_ref(), wallet.key.as_ref(), &[pbump]])?;
    create_pda(wallet, collateral, system, &spl_token::ID, spl_token::state::Account::LEN, 0, &[SEED_COLLATERAL, market.key.as_ref(), wallet.key.as_ref(), &[cbump]])?;
    invoke(
        &spl_token::instruction::initialize_account3(&spl_token::ID, collateral.key, mint.key, trader_ai.key)?,
        &[collateral.clone(), mint.clone(), token.clone()],
    )?;
    let t = TraderState {
        magic: TRADER_MAGIC,
        market: *market.key,
        wallet: *wallet.key,
        portfolio: pk,
        collateral: ck,
        bump,
        has_pending: 0,
        _pad: [0; 6],
        pending_vault: Pubkey::default(),
        pending_id: 0,
    };
    store(trader_ai, &t)?;
    invoke_signed(
        &perc::init_portfolio(trader_ai.key, market.key, portfolio.key),
        &[trader_ai.clone(), market.clone(), portfolio.clone(), percolator.clone()],
        &[trader_seeds!(t)],
    )
}

/// Moves collateral from the wallet into the trader's Percolator portfolio.
///
/// Accounts: 0 wallet [s], 1 trader, 2 wallet token account [w], 3 collateral [w], 4 portfolio [w],
/// 5 market [w], 6 Percolator collateral vault [w], 7 token program, 8 Percolator program.
fn deposit(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let wallet = acc(accounts, 0)?;
    let trader_ai = acc(accounts, 1)?;
    let source = acc(accounts, 2)?;
    let collateral = acc(accounts, 3)?;
    let portfolio = acc(accounts, 4)?;
    let market = acc(accounts, 5)?;
    let perc_vault = acc(accounts, 6)?;
    let token = acc(accounts, 7)?;
    let percolator = acc(accounts, 8)?;
    signer(wallet)?;
    key_is(token, &spl_token::ID)?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let t = load_trader(program_id, trader_ai, wallet.key)?;
    key_is(collateral, &t.collateral)?;
    key_is(portfolio, &t.portfolio)?;
    key_is(market, &t.market)?;
    if amount == 0 {
        return Err(RouterError::InvalidInstruction.into());
    }
    invoke(
        &spl_token::instruction::transfer(&spl_token::ID, source.key, collateral.key, wallet.key, &[], amount)?,
        &[source.clone(), collateral.clone(), wallet.clone(), token.clone()],
    )?;
    let view = perc::read_portfolio(portfolio, market.key, trader_ai.key)?;
    invoke_signed(
        &perc::deposit(trader_ai.key, market.key, portfolio.key, collateral.key, perc_vault.key, view.portfolio_id, view.sequence, amount as u128),
        &[trader_ai.clone(), market.clone(), portfolio.clone(), collateral.clone(), perc_vault.clone(), token.clone(), percolator.clone()],
        &[trader_seeds!(t)],
    )
}

/// Moves collateral from the trader's portfolio back to a token account the wallet owns. Refused
/// while a request is pending, so margin cannot leave between a request and its fill.
///
/// Accounts: 0 wallet [s], 1 trader, 2 portfolio [w], 3 market [w], 4 collateral [w],
/// 5 destination token account [w] (owned by the wallet), 6 Percolator collateral vault [w],
/// 7 Percolator vault authority, 8 token program, 9 Percolator program.
fn withdraw(program_id: &Pubkey, accounts: &[AccountInfo], amount: u64) -> ProgramResult {
    let wallet = acc(accounts, 0)?;
    let trader_ai = acc(accounts, 1)?;
    let portfolio = acc(accounts, 2)?;
    let market = acc(accounts, 3)?;
    let collateral = acc(accounts, 4)?;
    let dest = acc(accounts, 5)?;
    let perc_vault = acc(accounts, 6)?;
    let perc_vault_auth = acc(accounts, 7)?;
    let token = acc(accounts, 8)?;
    let percolator = acc(accounts, 9)?;
    signer(wallet)?;
    key_is(token, &spl_token::ID)?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let t = load_trader(program_id, trader_ai, wallet.key)?;
    key_is(collateral, &t.collateral)?;
    key_is(portfolio, &t.portfolio)?;
    key_is(market, &t.market)?;
    if t.has_pending != 0 {
        return Err(RouterError::RequestPending.into());
    }
    {
        let d = dest.try_borrow_data().map_err(|_| RouterError::BadAccount)?;
        let a = spl_token::state::Account::unpack(&d).map_err(|_| RouterError::BadAccount)?;
        if dest.owner != &spl_token::ID || a.owner != *wallet.key || a.mint != perc::market_collateral_mint(market)? {
            return Err(RouterError::BadAccount.into());
        }
    }
    let view = perc::read_portfolio(portfolio, market.key, trader_ai.key)?;
    let before = token_amount(collateral)?;
    invoke_signed(
        &perc::withdraw(trader_ai.key, market.key, portfolio.key, collateral.key, perc_vault.key, perc_vault_auth.key, view.portfolio_id, view.sequence, amount as u128),
        &[trader_ai.clone(), market.clone(), portfolio.clone(), collateral.clone(), perc_vault.clone(), perc_vault_auth.clone(), token.clone(), percolator.clone()],
        &[trader_seeds!(t)],
    )?;
    let received = token_amount(collateral)?.checked_sub(before).ok_or(RouterError::Overflow)?;
    invoke_signed(
        &spl_token::instruction::transfer(&spl_token::ID, collateral.key, dest.key, trader_ai.key, &[], received)?,
        &[collateral.clone(), dest.clone(), trader_ai.clone(), token.clone()],
        &[trader_seeds!(t)],
    )
}

// ---- the book ----

/// Creates a vault's book (its queue of pending requests and its mark). Anyone can.
///
/// Accounts: 0 payer [s, w], 1 vault, 2 book [w], 3 system program.
fn open_book(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let payer = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let book_ai = acc(accounts, 2)?;
    let system = acc(accounts, 3)?;
    signer(payer)?;
    writable(book_ai)?;
    key_is(system, &system_program::ID)?;
    let v = load_vault(vault_ai)?;
    let (bk, bump) = book_address(program_id, vault_ai.key);
    key_is(book_ai, &bk)?;
    create_pda(payer, book_ai, system, program_id, BOOK_LEN, 0, &[SEED_BOOK, vault_ai.key.as_ref(), &[bump]])?;
    let mut b = Book::zeroed();
    b.magic = BOOK_MAGIC;
    b.vault = *vault_ai.key;
    b.market = v.market;
    b.bump = bump;
    store(book_ai, &b)
}

/// Stressed margin check: after the trade, equity must cover initial margin plus fees on every
/// position with prices `STRESS_BPS` higher, and a `STRESS_BPS` loss on all of them.
fn margin_covers(market: &[u8], portfolio: &[u8], asset: u16, size: i128) -> Result<bool, ProgramError> {
    let (im_bps, min_im, fee_bps) = perc::margin_params(market)?;
    let (capital, pnl, _) = perc::portfolio_exposure(portfolio)?;
    let legs: Vec<(u16, i128)> = perc::positions(market, portfolio)?.iter().map(|p| (p.asset, p.size)).collect();
    let notional = |a: u16, q: u128| -> Result<u128, ProgramError> {
        let (_, price, _) = perc::asset_price(market, a)?;
        Ok(q.checked_mul(price as u128).ok_or(RouterError::Overflow)? / POS_SCALE)
    };
    // The position on the traded asset after the fill. A trade that only shrinks it (without
    // crossing zero) takes risk off and is never refused for margin: refusing it would stop a
    // trader with thin margin from closing.
    let held = legs.iter().find(|(a, _)| *a == asset).map_or(0, |(_, q)| *q);
    let after = held.checked_add(size).ok_or(RouterError::Overflow)?;
    if after == 0 || ((after > 0) == (held > 0) && held != 0 && after.unsigned_abs() < held.unsigned_abs()) {
        return Ok(true);
    }
    let mut total = notional(asset, after.unsigned_abs())?;
    for (a, q) in legs {
        if a != asset {
            total = total.checked_add(notional(a, q.unsigned_abs())?).ok_or(RouterError::Overflow)?;
        }
    }
    let stressed = total * (10_000 + STRESS_BPS) / 10_000;
    let need = stressed * im_bps as u128 / 10_000
        + total * STRESS_BPS / 10_000
        + stressed * fee_bps as u128 / 10_000
        + min_im;
    let equity = (capital as i128).checked_add(pnl).ok_or(RouterError::Overflow)?;
    Ok(equity >= 0 && equity as u128 >= need)
}

/// True when the account can be liquidated through the router: every position is settled at the
/// current price (so capital and PnL reflect it) and equity is below `LIQUIDATION_BPS` of the
/// positions' notional.
fn liquidatable(market: &[u8], portfolio: &[u8]) -> Result<bool, ProgramError> {
    let (capital, pnl, _) = perc::portfolio_exposure(portfolio)?;
    let mut notional = 0u128;
    for p in perc::positions(market, portfolio)? {
        if !p.settled {
            return Ok(false);
        }
        let (_, price, _) = perc::asset_price(market, p.asset)?;
        notional = notional
            .checked_add(p.size.unsigned_abs().checked_mul(price as u128).ok_or(RouterError::Overflow)? / POS_SCALE)
            .ok_or(RouterError::Overflow)?;
    }
    let equity = (capital as i128).checked_add(pnl).ok_or(RouterError::Overflow)?;
    Ok(notional != 0 && (equity <= 0 || (equity as u128).saturating_mul(10_000) < notional.saturating_mul(LIQUIDATION_BPS)))
}

/// Queues a trade. It fills at the first Pyth price published at or after its target time, which
/// is set here and cannot be changed or cancelled.
///
/// Accounts: 0 wallet [s, w], 1 trader [w], 2 vault, 3 book [w], 4 request [w], 5 market,
/// 6 trader portfolio, 7 system program. Data: size (i128, + = buy).
fn request(program_id: &Pubkey, accounts: &[AccountInfo], size: i128) -> ProgramResult {
    let wallet = acc(accounts, 0)?;
    let trader_ai = acc(accounts, 1)?;
    let vault_ai = acc(accounts, 2)?;
    let book_ai = acc(accounts, 3)?;
    let request_ai = acc(accounts, 4)?;
    let market = acc(accounts, 5)?;
    let portfolio = acc(accounts, 6)?;
    let system = acc(accounts, 7)?;
    signer(wallet)?;
    for w in [wallet, trader_ai, book_ai, request_ai] {
        writable(w)?;
    }
    key_is(system, &system_program::ID)?;
    let mut t = load_trader(program_id, trader_ai, wallet.key)?;
    let v = load_vault(vault_ai)?;
    let mut b: Book = load(book_ai, program_id, BOOK_MAGIC)?;
    key_is(market, &v.market)?;
    key_is(market, &t.market)?;
    key_is(portfolio, &t.portfolio)?;
    if b.vault != *vault_ai.key {
        return Err(RouterError::BadAccount.into());
    }
    if t.has_pending != 0 {
        return Err(RouterError::RequestPending.into());
    }
    if size == 0 || size == i128::MIN {
        return Err(RouterError::InvalidInstruction.into());
    }
    if b.len as usize >= MAX_PENDING {
        return Err(RouterError::BookFull.into());
    }
    perc::expect_market(market)?;
    if perc::asset_close_only(&market.try_borrow_data()?, v.asset_index)? {
        return Err(RouterError::CloseOnly.into());
    }
    if !margin_covers(&market.try_borrow_data()?, &portfolio.try_borrow_data()?, v.asset_index, size)? {
        return Err(RouterError::InsufficientMargin.into());
    }
    enqueue(program_id, wallet, wallet.key, trader_ai, &mut t, vault_ai, book_ai, &mut b, request_ai, system, size)
}

/// Stores a request for `owner`'s trading account (paid for by `payer`) and adds it to the book.
/// `size` 0 marks a close-out (see `request_close`).
#[allow(clippy::too_many_arguments)]
fn enqueue<'a>(
    program_id: &Pubkey,
    payer: &AccountInfo<'a>,
    owner: &Pubkey,
    trader_ai: &AccountInfo<'a>,
    t: &mut TraderState,
    vault_ai: &AccountInfo<'a>,
    book_ai: &AccountInfo<'a>,
    b: &mut Book,
    request_ai: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    size: i128,
) -> ProgramResult {
    let clock = Clock::get()?;
    let target = clock.unix_timestamp.max(b.mark_publish_time).checked_add(DELAY_SECS).ok_or(RouterError::Overflow)?;
    let id = b.next_id;
    let (rk, rbump) = request_address(program_id, vault_ai.key, id);
    key_is(request_ai, &rk)?;
    create_pda(payer, request_ai, system, program_id, REQUEST_LEN, BOND_LAMPORTS, &[SEED_REQUEST, vault_ai.key.as_ref(), &id.to_le_bytes(), &[rbump]])?;
    store(
        request_ai,
        &Request { magic: REQUEST_MAGIC, vault: *vault_ai.key, wallet: *owner, id, size, target_time: target, created_slot: clock.slot, bump: rbump, _pad: [0; 7] },
    )?;
    b.next_id = id.checked_add(1).ok_or(RouterError::Overflow)?;
    let (mut ids, mut targets) = (b.pending_id, b.pending_target);
    ids[b.len as usize] = id;
    targets[b.len as usize] = target;
    b.pending_id = ids;
    b.pending_target = targets;
    b.len += 1;
    store(book_ai, &*b)?;
    t.has_pending = 1;
    t.pending_vault = *vault_ai.key;
    t.pending_id = id;
    store(trader_ai, &*t)
}

/// Queues the forced close of a trader's position on a vault's market. Anyone may queue it (paying
/// the bond and the request's rent, which goes to the trader when it closes), in two cases:
///
/// - The market is close-only. Percolator refuses every risk-increasing trade on such a market,
///   so the vault cannot take the other side of a close; the fill reduces the position
///   unilaterally instead (Percolator's `RebalanceReduce`). The market only reopens once every
///   position on it is closed.
/// - The trader is below `LIQUIDATION_BPS` (see `liquidatable`). The fill closes the position
///   against the vault, if the account is still below that level at the fill price.
///
/// Either way it executes at the same price as any request: the first Pyth price published at or
/// after its target time.
///
/// Accounts: 0 payer [s, w], 1 trader [w], 2 trader wallet, 3 vault, 4 book [w], 5 request [w],
/// 6 market, 7 system program, 8 trader portfolio.
fn request_close(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let payer = acc(accounts, 0)?;
    let trader_ai = acc(accounts, 1)?;
    let wallet = acc(accounts, 2)?;
    let vault_ai = acc(accounts, 3)?;
    let book_ai = acc(accounts, 4)?;
    let request_ai = acc(accounts, 5)?;
    let market = acc(accounts, 6)?;
    let system = acc(accounts, 7)?;
    signer(payer)?;
    for w in [payer, trader_ai, book_ai, request_ai] {
        writable(w)?;
    }
    key_is(system, &system_program::ID)?;
    let mut t = load_trader(program_id, trader_ai, wallet.key)?;
    let v = load_vault(vault_ai)?;
    let mut b: Book = load(book_ai, program_id, BOOK_MAGIC)?;
    key_is(market, &v.market)?;
    key_is(market, &t.market)?;
    if b.vault != *vault_ai.key {
        return Err(RouterError::BadAccount.into());
    }
    if t.has_pending != 0 {
        return Err(RouterError::RequestPending.into());
    }
    if b.len as usize >= MAX_PENDING {
        return Err(RouterError::BookFull.into());
    }
    perc::expect_market(market)?;
    if !perc::asset_close_only(&market.try_borrow_data()?, v.asset_index)? {
        let portfolio = acc(accounts, 8)?;
        key_is(portfolio, &t.portfolio)?;
        if !liquidatable(&market.try_borrow_data()?, &portfolio.try_borrow_data()?)? {
            return Err(RouterError::NotCloseOnly.into());
        }
    }
    enqueue(program_id, payer, wallet.key, trader_ai, &mut t, vault_ai, book_ai, &mut b, request_ai, system, 0)
}

/// Moves the vault's mark to a verified Pyth price. The update must be newer than the current
/// mark. If a pending request's target is at or before it, it must be the first update at or
/// after the earliest such target: the mark never skips over the price a request fills at.
///
/// Accounts: 0 router authority, 1 vault, 2 book [w], 3 market [w], 4 Pyth update,
/// 5 vault program, 6 Percolator program. Data: the asset's next oracle observation sequence and
/// its authority epoch (Percolator checks both).
fn advance(program_id: &Pubkey, accounts: &[AccountInfo], observation_sequence: u64, authority_epoch: u64) -> ProgramResult {
    let authority = acc(accounts, 0)?;
    let vault_ai = acc(accounts, 1)?;
    let book_ai = acc(accounts, 2)?;
    let market = acc(accounts, 3)?;
    let pyth_ai = acc(accounts, 4)?;
    let vault_program = acc(accounts, 5)?;
    let percolator = acc(accounts, 6)?;
    writable(book_ai)?;
    writable(market)?;
    key_is(vault_program, &percolator_vault::id())?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let (ak, abump) = authority_address(program_id);
    key_is(authority, &ak)?;
    let v = load_vault(vault_ai)?;
    let mut b: Book = load(book_ai, program_id, BOOK_MAGIC)?;
    if b.vault != *vault_ai.key {
        return Err(RouterError::BadAccount.into());
    }
    key_is(market, &v.market)?;
    let u = pyth::read(pyth_ai, &v.oracle_feeds[0]).map_err(|_| RouterError::BadOracle)?;
    let price = u.price_e6().ok_or(RouterError::BadOracle)?;
    if u.publish_time <= b.mark_publish_time {
        return Err(RouterError::WrongUpdate.into());
    }
    if let Some(earliest) = b.earliest_target() {
        if earliest <= u.publish_time && !u.is_first_at_or_after(earliest) {
            return Err(RouterError::WrongUpdate.into());
        }
    }
    let mut data = vec![TAG_PUSH_MARK];
    data.extend_from_slice(&price.to_le_bytes());
    data.extend_from_slice(&observation_sequence.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    invoke_signed(
        &Instruction {
            program_id: percolator_vault::id(),
            accounts: vec![
                AccountMeta::new_readonly(ak, true),
                AccountMeta::new_readonly(*vault_ai.key, false),
                AccountMeta::new(*market.key, false),
                AccountMeta::new_readonly(PERCOLATOR_PROGRAM_ID, false),
            ],
            data,
        },
        &[authority.clone(), vault_ai.clone(), market.clone(), percolator.clone(), vault_program.clone()],
        &[&[SEED_AUTHORITY, &[abump]]],
    )?;
    b.mark_publish_time = u.publish_time;
    b.mark_prev_publish_time = u.prev_publish_time;
    b.mark_price = price;
    store(book_ai, &b)
}

/// Fills a request at the mark, once the mark is the first Pyth price at or after the request's
/// target and Percolator's effective price has reached it. Anyone can call it; the caller earns
/// the request's bond.
///
/// Accounts: 0 executor [s, w], 1 request [w], 2 trader [w], 3 trader wallet [w] (gets the
/// request's rent back), 4 book [w], 5 vault [w], 6 market [w], 7 trader portfolio [w],
/// 8 vault LP portfolio [w], 9 matcher delegate, 10 router authority, 11 vault program,
/// 12 Percolator program.
fn fill(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let executor = acc(accounts, 0)?;
    let request_ai = acc(accounts, 1)?;
    let trader_ai = acc(accounts, 2)?;
    let wallet = acc(accounts, 3)?;
    let book_ai = acc(accounts, 4)?;
    let vault_ai = acc(accounts, 5)?;
    let market = acc(accounts, 6)?;
    let portfolio = acc(accounts, 7)?;
    let lp_portfolio = acc(accounts, 8)?;
    let delegate = acc(accounts, 9)?;
    let authority = acc(accounts, 10)?;
    let vault_program = acc(accounts, 11)?;
    let percolator = acc(accounts, 12)?;
    signer(executor)?;
    for w in [executor, request_ai, trader_ai, wallet, book_ai, vault_ai, market, portfolio, lp_portfolio] {
        writable(w)?;
    }
    key_is(vault_program, &percolator_vault::id())?;
    key_is(percolator, &PERCOLATOR_PROGRAM_ID)?;
    let (ak, abump) = authority_address(program_id);
    key_is(authority, &ak)?;
    let r: Request = load(request_ai, program_id, REQUEST_MAGIC)?;
    let mut t = load_trader(program_id, trader_ai, &r.wallet)?;
    key_is(wallet, &r.wallet)?;
    let mut b: Book = load(book_ai, program_id, BOOK_MAGIC)?;
    let v = load_vault(vault_ai)?;
    if r.vault != *vault_ai.key || b.vault != *vault_ai.key || t.pending_vault != *vault_ai.key || t.pending_id != r.id {
        return Err(RouterError::BadAccount.into());
    }
    key_is(market, &v.market)?;
    key_is(portfolio, &t.portfolio)?;
    key_is(lp_portfolio, &v.lp_portfolio)?;
    key_is(delegate, &v.matcher_delegate)?;

    // The mark is the unique first Pyth price at or after the target, and Percolator's price is it.
    if !(b.mark_prev_publish_time < r.target_time && r.target_time <= b.mark_publish_time) {
        return Err(RouterError::NotAtTarget.into());
    }
    perc::expect_market(market)?;
    let (market_id, effective, _) = perc::asset_price(&market.try_borrow_data()?, v.asset_index)?;
    if effective != b.mark_price {
        return Err(RouterError::NotConverged.into());
    }
    let (_, _, fee_bps) = perc::margin_params(&market.try_borrow_data()?)?;

    let held = |p: &AccountInfo, m: &AccountInfo| -> Result<i128, ProgramError> {
        Ok(perc::positions(&m.try_borrow_data()?, &p.try_borrow_data()?)?.iter().find(|x| x.asset == v.asset_index).map_or(0, |x| x.size))
    };
    let before = held(portfolio, market)?;
    // A forced close (size 0) trades whatever closes the position, or nothing at all.
    let mut size = r.size;
    if r.size == 0 {
        let close_only = perc::asset_close_only(&market.try_borrow_data()?, v.asset_index)?;
        if close_only && before != 0 {
            // Reduce the whole position unilaterally at Percolator's price, which is the mark.
            let view = perc::read_portfolio(portfolio, market.key, trader_ai.key)?;
            invoke_signed(
                &perc::rebalance_reduce(trader_ai.key, market.key, portfolio.key, view.portfolio_id, view.position_epoch, v.asset_index, u128::MAX >> 1),
                &[trader_ai.clone(), market.clone(), portfolio.clone(), percolator.clone()],
                &[trader_seeds!(t)],
            )?;
        } else if !close_only && before != 0 && liquidatable(&market.try_borrow_data()?, &portfolio.try_borrow_data()?)? {
            // Still below the liquidation level at the fill price: close against the vault.
            size = before.checked_neg().ok_or(RouterError::Overflow)?;
        }
    }
    if size == 0 {
        msg!("fill {} {} {}", { r.id }, held(portfolio, market)? - before, effective);
        b.remove(r.id)?;
        store(book_ai, &b)?;
        t.has_pending = 0;
        store(trader_ai, &t)?;
        return close(request_ai, BOND_LAMPORTS, executor, wallet);
    }

    let mut arm = vec![TAG_ARM_FILL];
    arm.extend_from_slice(&size.to_le_bytes());
    invoke_signed(
        &Instruction {
            program_id: percolator_vault::id(),
            accounts: vec![AccountMeta::new_readonly(ak, true), AccountMeta::new(*vault_ai.key, false)],
            data: arm,
        },
        &[authority.clone(), vault_ai.clone(), vault_program.clone()],
        &[&[SEED_AUTHORITY, &[abump]]],
    )?;
    let taker_view = perc::read_portfolio(portfolio, market.key, trader_ai.key)?;
    let lp_view = perc::read_portfolio(lp_portfolio, market.key, vault_ai.key)?;
    invoke_signed(
        &perc::trade_cpi(
            trader_ai.key,
            market.key,
            portfolio.key,
            lp_portfolio.key,
            &percolator_vault::id(),
            vault_ai.key,
            delegate.key,
            &taker_view,
            &lp_view,
            v.asset_index,
            market_id,
            size,
            fee_bps,
        ),
        &[trader_ai.clone(), market.clone(), portfolio.clone(), lp_portfolio.clone(), vault_program.clone(), vault_ai.clone(), delegate.clone(), percolator.clone()],
        &[trader_seeds!(t)],
    )?;
    // What actually traded (the vault may fill less than requested) and at what price.
    msg!("fill {} {} {}", { r.id }, held(portfolio, market)? - before, effective);

    b.remove(r.id)?;
    store(book_ai, &b)?;
    t.has_pending = 0;
    store(trader_ai, &t)?;
    close(request_ai, BOND_LAMPORTS, executor, wallet)
}

/// Removes a request nobody filled within `GRACE_SECS` of its target. Its bond is forfeited
/// (kept in the book account, which nothing can withdraw); its rent goes back to the trader.
///
/// Accounts: 0 caller [s], 1 request [w], 2 trader [w], 3 trader wallet [w], 4 book [w].
fn expire(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let caller = acc(accounts, 0)?;
    let request_ai = acc(accounts, 1)?;
    let trader_ai = acc(accounts, 2)?;
    let wallet = acc(accounts, 3)?;
    let book_ai = acc(accounts, 4)?;
    signer(caller)?;
    for w in [request_ai, trader_ai, wallet, book_ai] {
        writable(w)?;
    }
    let r: Request = load(request_ai, program_id, REQUEST_MAGIC)?;
    let mut t = load_trader(program_id, trader_ai, &r.wallet)?;
    key_is(wallet, &r.wallet)?;
    let mut b: Book = load(book_ai, program_id, BOOK_MAGIC)?;
    if b.vault != r.vault || t.pending_vault != r.vault || t.pending_id != r.id {
        return Err(RouterError::BadAccount.into());
    }
    if Clock::get()?.unix_timestamp <= r.target_time.saturating_add(GRACE_SECS) {
        return Err(RouterError::TooEarly.into());
    }
    b.remove(r.id)?;
    b.forfeited_bonds = b.forfeited_bonds.saturating_add(BOND_LAMPORTS);
    store(book_ai, &b)?;
    t.has_pending = 0;
    store(trader_ai, &t)?;
    close(request_ai, BOND_LAMPORTS, book_ai, wallet)
}
