//! Adversarial tests. Each one attempts an attack against the vault on top of the production
//! Percolator binary and asserts it fails without moving value, or is harmless by design.

mod common;
use common::*;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_vault::{error::VaultError, percolator as perc, processor, state};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
};

fn code(e: VaultError) -> String {
    format!("custom program error: {:#x}", e as u32)
}

fn expect_err(r: Result<u64, String>, e: VaultError) {
    let msg = r.expect_err("attack unexpectedly succeeded");
    assert!(msg.contains(&code(e)), "expected {e:?}, got:\n{msg}");
}

/// A funded vault with Alice as the only holder, rolled into epoch 1.
fn funded() -> (World, User) {
    let mut w = World::new();
    w.env.warp_to_slot(5);
    w.operate();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 0).unwrap();
    (w, alice)
}

fn matcher_request(asset: u16, size: i128) -> Vec<u8> {
    let mut d = vec![0u8; 67];
    d[1..9].copy_from_slice(&7u64.to_le_bytes());
    d[9..11].copy_from_slice(&asset.to_le_bytes());
    d[19..27].copy_from_slice(&INITIAL_PRICE.to_le_bytes());
    d[27..43].copy_from_slice(&size.to_le_bytes());
    d
}

// ---------------------------------------------------------------- matcher

#[test]
fn direct_matcher_call_is_rejected() {
    let (mut w, _) = funded();
    let attacker = Keypair::new();
    w.env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let before = w.vault_state();
    let ix = Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new_readonly(attacker.pubkey(), true),
            AccountMeta::new(w.vault, false),
        ],
        data: matcher_request(0, 5 * UNIT as i128),
    };
    expect_err(w.send(vec![ix], &[&attacker]), VaultError::UnauthorizedMatcherCaller);
    assert_eq!({ w.vault_state().inventory }, { before.inventory });
}

#[test]
fn foreign_lp_cannot_route_fills_through_the_vault() {
    // Actor 2 points its own LP portfolio at the vault program and the vault account as context.
    let (mut w, _) = funded();
    let lp = 2usize;
    let lp_key = w.env.actors[lp].signer.insecure_clone();
    let lp_portfolio = w.env.actors[lp].portfolio;
    let delegate = perc::matcher_delegate(&w.env.market, &lp_portfolio, &lp_key.pubkey(), &pid(), &w.vault);
    let cfg = Instruction {
        program_id: perc::PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(lp_key.pubkey(), true),
            AccountMeta::new_readonly(w.env.market, false),
            AccountMeta::new(lp_portfolio, false),
            AccountMeta::new_readonly(pid(), false),
            AccountMeta::new_readonly(w.vault, false),
            AccountMeta::new_readonly(delegate, false),
        ],
        data: ProgIx::SetMatcherConfig {
            portfolio_id: w.env.primary_portfolio_id(lp),
            expected_sequence: w.env.primary_portfolio_matcher_sequence(lp),
            position_epoch: w.env.primary_portfolio_position_epoch(lp),
            asset_generation_frontier: w.frontier(),
            enabled: 1,
            trade_fee_cap_bps: 10_000,
            expiry_slot: u64::MAX,
        }
        .encode(),
    };
    w.send(vec![cfg], &[&lp_key]).expect("Percolator lets any LP name any matcher");
    let market_id = w.env.primary_market_state().1.assets[0].market_id;
    let taker = w.env.actors[0].signer.insecure_clone();
    let trade = Instruction {
        program_id: perc::PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(taker.pubkey(), true),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new(w.env.actors[0].portfolio, false),
            AccountMeta::new(lp_portfolio, false),
            AccountMeta::new_readonly(pid(), false),
            AccountMeta::new(w.vault, false),
            AccountMeta::new_readonly(delegate, false),
        ],
        data: ProgIx::TradeCpi {
            account_a_portfolio_id: w.env.primary_portfolio_id(0),
            account_a_position_epoch: w.env.primary_portfolio_position_epoch(0),
            account_b_portfolio_id: w.env.primary_portfolio_id(lp),
            account_b_position_epoch: w.env.primary_portfolio_position_epoch(lp),
            account_b_matcher_sequence: w.env.primary_portfolio_matcher_sequence(lp),
            asset_index: 0,
            market_id,
            size_q: 5 * UNIT as i128,
            fee_bps: 0,
            limit_price: 0,
            backing_fee_cap_bps: 0,
        }
        .encode(),
    };
    let before = w.vault_state();
    expect_err(w.send(vec![trade], &[&taker]), VaultError::UnauthorizedMatcherCaller);
    assert_eq!({ w.vault_state().inventory }, { before.inventory });
}

#[test]
fn inventory_cap_limits_fills() {
    // 10 USDC of NAV at 1.00: each fill up to 7.5 units (0.75x NAV), position up to 30 (3x).
    // 10% margin, so the vault's own caps bind before Percolator's margin check.
    let mut w = World::with_config(devnet_like_config());
    w.env.warp_to_slot(5);
    w.operate();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 10_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    let per_fill = 7_500_000i128;
    w.taker_trade(0, 10 * UNIT as i128).unwrap();
    assert_eq!({ w.vault_state().inventory }, -per_fill, "per-fill cap");
    for _ in 0..3 {
        w.taker_trade(0, 10 * UNIT as i128).unwrap();
    }
    assert_eq!({ w.vault_state().inventory }, -30 * UNIT as i128, "position cap");
    w.taker_trade(0, 10 * UNIT as i128).unwrap();
    assert_eq!({ w.vault_state().inventory }, -30 * UNIT as i128, "nothing past the position cap");
    // Selling back (reducing the vault's short) is always allowed.
    w.taker_trade(0, -5 * UNIT as i128).unwrap();
    assert_eq!({ w.vault_state().inventory }, -25 * UNIT as i128);
}

#[test]
fn roll_needs_flat_portfolio_and_reduce_only_gets_there() {
    let (mut w, alice) = funded();
    w.taker_trade(0, TEN_M).unwrap(); // vault short 10 units
    // A withdrawal of half the vault: more than the cash it keeps outside Percolator.
    let half = w.tokens(alice.shares) / 2;
    w.withdraw(&alice, half).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    expect_err(w.roll(), VaultError::NotFlat);
    w.require_flat().unwrap();

    // Epoch over with withdrawals that need the vault flat: the vault only reduces. A taker buy
    // would grow the short, so it fills nothing; a taker sell shrinks it and fills.
    w.taker_trade(0, TEN_M).unwrap();
    assert_eq!({ w.vault_state().inventory }, -TEN_M, "no growth in reduce-only");
    w.taker_trade(0, -2 * TEN_M).unwrap();
    assert_eq!({ w.vault_state().inventory }, 0, "fill capped at flat, never crosses");
    w.roll().unwrap();
    assert_eq!({ w.vault_state().epoch }, 2);
}

#[test]
fn matcher_approval_expires_and_anyone_can_renew_it() {
    let (mut w, _) = funded();
    // The approval lasts CANON_MATCHER_TTL_SLOTS (200 in test builds) and is renewed at rolls.
    w.advance(processor::CANON_MATCHER_TTL_SLOTS + 10, INITIAL_PRICE);
    // The request lands; its fill is refused while the approval is expired.
    let id = w.request(0, TEN_M).unwrap();
    let target = w.request_target(id);
    let slot = w.env.current_slot() + 1;
    w.env.set_clock(slot, target);
    let prev = w.pyth_time;
    w.pyth_time = target;
    let u = w.pyth_update(w.price, target, prev);
    w.advance_to(u).unwrap();
    w.converge();
    assert!(w.fill(0, id).is_err(), "expired approval refuses fills");
    let mut data = vec![processor::TAG_REFRESH_MATCHER];
    data.extend_from_slice(&w.frontier().to_le_bytes());
    let refresh = Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new_readonly(w.vault, false),
            AccountMeta::new_readonly(w.env.market, false),
            AccountMeta::new(w.portfolio, false),
            AccountMeta::new_readonly(pid(), false),
            AccountMeta::new_readonly(w.delegate, false),
            AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
        ],
        data,
    };
    // No signer beyond the fee payer: renewal is permissionless.
    w.send(vec![refresh], &[]).unwrap();
    w.fill(0, id).unwrap();
    assert_eq!({ w.vault_state().inventory }, -TEN_M);
}

use percolator_vault::processor::InitParams;

// ---------------------------------------------------------------- epochs and claims

#[test]
fn roll_before_epoch_end_fails() {
    let (mut w, _) = funded();
    expect_err(w.roll(), VaultError::EpochNotOver);
}

#[test]
fn claims_cannot_be_early_doubled_stolen_or_forged() {
    let (mut w, alice) = funded();
    let bob = w.new_user(100_000_000);
    w.deposit(&bob, 10_000_000).unwrap();
    expect_err(w.claim(&bob, 1), VaultError::EpochNotRolled);
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();

    // Mallory signs with Bob's ticket.
    let mallory = w.new_user(0);
    let mut ix = w.claim_ix(&mallory, 1);
    ix.accounts[2] = AccountMeta::new(w.ticket(&bob.kp.pubkey()), false);
    expect_err(w.send(vec![ix], &[&mallory.kp]), VaultError::BadAccount);

    // Bob presents the record of a different epoch.
    let ix = w.claim_ix(&bob, 0);
    expect_err(w.send(vec![ix], &[&bob.kp]), VaultError::BadAccount);

    w.claim(&bob, 1).unwrap();
    expect_err(w.claim(&bob, 1), VaultError::NothingToClaim);
    // Alice's shares are untouched by all of this.
    assert_eq!(w.tokens(alice.shares), 50_000_000_000);
}

#[test]
fn new_request_requires_claiming_the_old_one() {
    let (mut w, _) = funded();
    let bob = w.new_user(100_000_000);
    w.deposit(&bob, 1_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    expect_err(w.deposit(&bob, 1_000_000), VaultError::ClaimFirst);
    w.claim(&bob, 1).unwrap();
    w.deposit(&bob, 1_000_000).unwrap();
}

#[test]
fn deposit_into_a_fake_buffer_is_rejected() {
    let (mut w, alice) = funded();
    let fake = Pubkey::new_unique();
    let (mint, owner) = (w.env.mint, alice.kp.pubkey());
    set_token(&mut w.env.svm, fake, mint, owner, 0);
    let mut ix = w.deposit_ix(&alice, 1_000);
    ix.accounts[4] = AccountMeta::new(fake, false);
    expect_err(w.send(vec![ix], &[&alice.kp]), VaultError::BadAccount);
}

#[test]
fn donation_is_harmless_and_accrues_to_holders() {
    let (mut w, alice) = funded();
    // Someone sends collateral straight to the buffer.
    let buffer = w.buffer;
    let (mint, vault) = (w.env.mint, w.vault);
    set_token(&mut w.env.svm, buffer, mint, vault, 7_000_000);
    w.withdraw(&alice, w.tokens(alice.shares)).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 1).unwrap();
    let got = w.tokens(alice.collateral);
    assert!(got > 100_000_000 && got <= 107_000_000, "sole holder receives the donation: {got}");
    assert_eq!(w.tokens(w.buffer), { w.vault_state().reserved_assets });
}

#[test]
fn inflation_attack_on_first_depositor_fails() {
    // Attacker is first with 1 atom, then donates to the buffer to inflate the share price
    // before the victim's deposit is priced in the next epoch.
    let mut w = World::new();
    w.env.warp_to_slot(5);
    w.operate();
    let attacker = w.new_user(20_000_000);
    let victim = w.new_user(10_000_000);
    w.deposit(&attacker, 1).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&attacker, 0).unwrap();
    w.deposit(&victim, 10_000_000).unwrap();
    let (buffer, mint, vault) = (w.buffer, w.env.mint, w.vault);
    set_token(&mut w.env.svm, buffer, mint, vault, 10_000_000 + 10_000_000); // donation of 10M
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&victim, 1).unwrap();
    assert!(w.tokens(victim.shares) > 0, "victim is never rounded to zero shares");
    // Both exit; the attacker must not end with more than they put in (1 + 10M donation).
    w.withdraw(&attacker, w.tokens(attacker.shares)).unwrap();
    w.withdraw(&victim, w.tokens(victim.shares)).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&attacker, 2).unwrap();
    w.claim(&victim, 2).unwrap();
    let attacker_out = w.tokens(attacker.collateral) - (20_000_000 - 1);
    let victim_out = w.tokens(victim.collateral);
    println!("attacker out {attacker_out}, victim out {victim_out}");
    assert!(attacker_out < 1 + 10_000_000, "the attack loses money");
    assert!(victim_out >= 10_000_000 - 10_000, "the victim keeps (almost) everything");
}

// ---------------------------------------------------------------- creation

#[test]
fn creation_rejects_bad_params_duplicates_and_wrong_mint() {
    let mut w = World::new();
    w.env.warp_to_slot(5);
    // Attach mode (quoting on an asset whose price the vault does not control) is refused: its
    // fills could not be sequenced by the router. So is an unknown mode.
    for bad in [default_params(1), InitParams { mode: 7, ..default_params(1) }] {
        let e = w.create_vault(bad).unwrap_err();
        assert!(e.contains(&code(VaultError::InvalidParams)), "{bad:?}: {e}");
    }

    // Pre-funding the canonical vault address does not block creation.
    let (vault, _) = state::canonical_vault_address(&pid(), &w.env.market, &FEED);
    w.env.svm.airdrop(&vault, 12_345).unwrap();
    w.create_vault(operate_params(1)).unwrap();
    // A second creation for the same feed fails.
    assert!(w.create_vault(operate_params(1)).is_err());

    // Wrong collateral mint.
    let mut p = InitParams { oracle_feeds: [[8; 32], [0; 32], [0; 32]], ..operate_params(2) };
    p.asset_generation_frontier = w.frontier();
    let mut ix = w.init_ix(&p);
    let fake_mint = Pubkey::new_unique();
    ix.accounts[4] = AccountMeta::new_readonly(fake_mint, false);
    let creator = w.creator.insecure_clone();
    expect_err(w.send(vec![ix], &[&creator]), VaultError::BadAccount);
}

#[test]
fn listing_twice_fails_and_pending_vault_cannot_trade() {
    let mut w = World::new();
    w.env.set_clock(5, 1_000);
    w.env.update_market_init_fee_policy(LISTING_FEE as u128).unwrap();
    w.create_vault(operate_params(3)).unwrap();
    let alice = w.new_user(100_000_000);
    // Until its asset is listed the vault accepts nothing and cannot roll or quote.
    expect_err(w.deposit(&alice, 1_000_000), VaultError::NotActive);
    expect_err(w.roll(), VaultError::NotActive);
    let lister = w.new_user(10_000_000);
    let pyth = w.env.set_pyth_price(&FEED, 100_000_000, -8, 0, 1_000);
    w.list_asset(&lister, pyth).unwrap();
    expect_err(w.list_asset(&lister, pyth), VaultError::AlreadyInitialized);
}

// ---------------------------------------------------------------- stray funds

#[test]
fn sweep_moves_stray_vault_owned_collateral_into_the_buffer() {
    let (mut w, _) = funded();
    let stray = Pubkey::new_unique();
    let (mint, vault) = (w.env.mint, w.vault);
    set_token(&mut w.env.svm, stray, mint, vault, 3_000);
    let before = w.tokens(w.buffer);
    let ix = Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new_readonly(w.vault, false),
            AccountMeta::new(stray, false),
            AccountMeta::new(w.buffer, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: vec![processor::TAG_SWEEP],
    };
    w.send(vec![ix.clone()], &[]).unwrap();
    assert_eq!(w.tokens(w.buffer), before + 3_000);
    assert_eq!(w.tokens(stray), 0);
    // A token account the vault does not own cannot be swept.
    let other = Pubkey::new_unique();
    let owner = Keypair::new().pubkey();
    set_token(&mut w.env.svm, other, mint, owner, 3_000);
    let mut ix2 = ix;
    ix2.accounts[1] = AccountMeta::new(other, false);
    expect_err(w.send(vec![ix2], &[]), VaultError::BadAccount);
}

/// The unwind deleverages the takers on the other side, and Percolator then takes no new risk on
/// the asset until every position on it is closed. The router closes them out (anyone can queue
/// it, at the next Pyth price like any request), after which the market trades again.
#[test]
fn a_market_is_closed_out_after_an_unwind_and_reopens() {
    use percolator_router::RouterError;
    let router_err = |r: Result<u64, String>, e: RouterError| assert!(r.unwrap_err().contains(&format!("0x{:x}", e as u32)));
    let (mut w, alice) = funded();
    // Closing out is only for a close-only market.
    w.taker_trade(0, TEN_M).unwrap();
    router_err(w.close_out(0), RouterError::NotCloseOnly);
    w.taker_trade(2, -4_000_000).unwrap(); // a second taker on the other side: the vault is short 6
    let half = w.tokens(alice.shares) / 2;
    w.withdraw(&alice, half).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.require_flat().unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    let unwind = Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new(w.vault, false),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new(w.portfolio, false),
            AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
        ],
        data: vec![processor::TAG_UNWIND],
    };
    w.send(vec![unwind], &[]).unwrap();
    assert!(w.portfolio_view().flat);
    w.roll().unwrap();
    let asset = |w: &World| w.env.primary_market_state().1.assets[w.asset() as usize];
    // Taker 0's long 10 was deleveraged to the 4 that taker 2 is still short.
    assert_eq!((asset(&w).oi_eff_long_q, asset(&w).oi_eff_short_q), (4_000_000, 4_000_000));
    assert_ne!(asset(&w).a_long, percolator::ADL_ONE);

    // New risk is refused at request time (it could never fill), for new and old takers alike.
    w.trader(1);
    router_err(w.request(1, 1_000_000), RouterError::CloseOnly);
    router_err(w.request(0, -1_000_000), RouterError::CloseOnly);

    // Anyone closes a remaining position out, at the first Pyth price after its target. Closing
    // one side deleverages the other, so here the long's close-out empties the market.
    let before: Vec<i128> = [0, 2].iter().map(|t| { let d = w.env.svm.get_account(&w.taker_portfolio(*t)).unwrap().data; let (c, p, _) = perc::portfolio_exposure(&d).unwrap(); c as i128 + p }).collect();
    w.close_out(0).unwrap();
    assert_eq!((asset(&w).oi_eff_long_q, asset(&w).oi_eff_short_q), (0, 0));
    for (i, t) in [0, 2].iter().enumerate() {
        let d = w.env.svm.get_account(&w.taker_portfolio(*t)).unwrap().data;
        let (c, p, _) = perc::portfolio_exposure(&d).unwrap();
        assert_eq!(c as i128 + p, before[i], "closed at the mark: no gain or loss at an unchanged price");
    }
    // With nothing left open the sides reset (once the emptied positions are settled), and the
    // market is no longer close-only.
    router_err(w.close_out(2), RouterError::NotCloseOnly);
    w.settle();
    w.finalize_resets();
    assert_eq!(asset(&w).a_long, percolator::ADL_ONE);
    assert_eq!(asset(&w).mode_long, percolator::SideModeV16::Normal);

    // The market is open again.
    w.taker_trade(1, 1_000_000).unwrap();
    w.taker_trade(0, -2_000_000).unwrap();
}

/// An epoch settles while the vault holds a position, at a price fixed in advance through the
/// router, and trading is not interrupted. The deposit buys shares at the vault's value at that
/// price (its open loss included), so neither the depositor nor the incumbents gain.
#[test]
fn an_epoch_settles_with_the_vault_holding_a_position() {
    let (mut w, alice) = funded();
    w.taker_trade(0, TEN_M).unwrap(); // the vault is short 10 units
    let bob = w.new_user(100_000_000);
    w.deposit(&bob, 20_000_000).unwrap();
    w.withdraw(&alice, w.tokens(alice.shares) / 50).unwrap(); // 2%: well within the cash reserve
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    // The price rises 5%: the vault's short has lost about 0.5 when the epoch settles.
    w.move_price(1, INITIAL_PRICE * 105 / 100);

    // Not flat, so the plain roll is refused, but nothing is closing-only: a new long fills.
    expect_err(w.roll(), VaultError::NotFlat);
    expect_err(w.require_flat(), VaultError::CanSettleOpen);
    w.taker_trade(1, 2_000_000).unwrap();
    let inventory = { w.vault_state().inventory };
    assert_eq!(inventory, -12_000_000);

    let (nav, supply) = (w.nav(), w.share_supply());
    assert!(nav < 50_000_000, "the open loss is in the value: {nav}");
    w.settle_open().unwrap();
    assert_eq!({ w.vault_state().epoch }, 2);
    assert_eq!({ w.vault_state().inventory }, inventory, "the position is untouched");
    assert!(!w.portfolio_view().flat);

    // One price for everyone: the value per share is the same before and after (to rounding).
    let (nav2, supply2) = (w.nav(), w.share_supply());
    let (before, after) = (nav as u128 * 1_000_000_000 / supply as u128, nav2 as u128 * 1_000_000_000 / supply2 as u128);
    assert!(before.abs_diff(after) <= before / 100_000, "share value {before} -> {after}");
    w.claim(&bob, 1).unwrap();
    w.claim(&alice, 1).unwrap();
    let bob_value = w.tokens(bob.shares) as u128 * nav2 as u128 / supply2 as u128;
    assert!(bob_value <= 20_000_000 && bob_value > 19_990_000, "bob holds what he paid for: {bob_value}");
    // The reserve stays as cash for the next epoch's withdrawals.
    let cash = w.tokens(w.buffer) - w.vault_state().reserved_assets;
    assert!(cash >= nav2 / 11, "cash reserve {cash} of {nav2}");
    // Trading carries on.
    w.taker_trade(0, -TEN_M).unwrap();
}

#[test]
fn only_the_router_settles_an_open_epoch_and_only_at_its_target_price() {
    let (mut w, _alice) = funded();
    w.taker_trade(0, TEN_M).unwrap();
    let bob = w.new_user(100_000_000);
    w.deposit(&bob, 20_000_000).unwrap();
    // No settlement request before the epoch is over.
    assert!(w.request_settle().is_err());
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    expect_err(w.roll(), VaultError::NotFlat);
    // A signer that is not the router's authority does not unlock the roll.
    let mallory = Keypair::new();
    w.env.svm.airdrop(&mallory.pubkey(), 1_000_000_000).unwrap();
    let mut forged = w.roll_ix(&w.payer.pubkey());
    forged.accounts.push(AccountMeta::new_readonly(mallory.pubkey(), true));
    expect_err(w.send(vec![forged], &[&mallory]), VaultError::NotRouter);
    // Queued, but the mark is not at the request's target price yet.
    let id = w.request_settle().unwrap();
    let ix = w.settle_ix(id);
    assert!(w.send(vec![ix], &[]).unwrap_err().contains(&format!("0x{:x}", percolator_router::RouterError::NotAtTarget as u32)));
    assert_eq!({ w.vault_state().epoch }, 1);
}

#[test]
fn withdrawals_beyond_the_cash_reserve_need_the_vault_flat() {
    let (mut w, alice) = funded();
    w.taker_trade(0, TEN_M).unwrap();
    w.withdraw(&alice, w.tokens(alice.shares) / 2).unwrap();
    // Not before the epoch is over.
    expect_err(w.require_flat(), VaultError::EpochNotOver);
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    // The open settlement cannot pay half the vault out of a 10% cash reserve.
    assert!(w.settle_open().unwrap_err().contains(&format!("0x{:x}", VaultError::NotFlat as u32)));
    assert_eq!({ w.vault_state().epoch }, 1);
    // The request that could not execute expires like any other (executors check first and do
    // not queue one that cannot succeed), forfeiting its bond.
    let id = { w.book().pending_id }[0];
    let target = w.request_target(id);
    let slot = w.env.current_slot() + 1;
    w.env.set_clock(slot, target + percolator_router::GRACE_SECS + 1);
    let payer = w.payer.pubkey();
    let router = percolator_router::id();
    let expire = Instruction {
        program_id: router,
        accounts: vec![
            AccountMeta::new_readonly(payer, true),
            AccountMeta::new(percolator_router::state::request_address(&router, &w.vault, id).0, false),
            AccountMeta::new_readonly(payer, false), // no trader behind a settlement request
            AccountMeta::new(payer, false),
            AccountMeta::new(percolator_router::state::book_address(&router, &w.vault).0, false),
        ],
        data: vec![percolator_router::TAG_EXPIRE],
    };
    w.send(vec![expire], &[]).unwrap();
    assert_eq!({ w.book().len }, 0);
    // So the vault goes closing-only, and settles once it is flat.
    w.taker_trade(1, 1_000_000).unwrap();
    assert_eq!({ w.vault_state().inventory }, -11_000_000, "still open for business until then");
    w.require_flat().unwrap();
    w.taker_trade(1, 1_000_000).unwrap();
    assert_eq!({ w.vault_state().inventory }, -11_000_000, "closing-only now");
}

#[allow(unused)]
fn unused() -> Pubkey {
    system_program::ID
}

#[test]
fn a_taker_holding_a_position_cannot_lock_withdrawals() {
    let (mut w, alice) = funded();
    // Mallory opens a position against the vault and never closes it.
    w.taker_trade(0, TEN_M).unwrap();
    let half = w.tokens(alice.shares) / 2;
    w.withdraw(&alice, half).unwrap();
    let unwind = |w: &World| Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new(w.vault, false),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new(w.portfolio, false),
            AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
        ],
        data: vec![processor::TAG_UNWIND],
    };
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    expect_err(w.roll(), VaultError::NotFlat);
    w.require_flat().unwrap();
    // One epoch of reduce-only grace first.
    expect_err(w.send(vec![unwind(&w)], &[]), VaultError::EpochNotOver);
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    let cu = w.send(vec![unwind(&w)], &[]).unwrap();
    println!("unwind CU: {cu}");
    assert!(w.portfolio_view().flat, "the vault closed its own position");
    w.roll().unwrap();
    w.claim(&alice, 1).unwrap();
    assert!(w.tokens(alice.collateral) > 50_000_000);
}
