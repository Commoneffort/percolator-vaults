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
    w.create_vault(default_params(1)).unwrap();
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
    let (mut w, _) = funded();
    let fill = 40 * UNIT as i128;
    w.taker_trade(0, fill).unwrap();
    assert_eq!({ w.vault_state().inventory }, -fill);
    // Cap is 45 units: the second 40-unit buy only fills 5.
    w.taker_trade(0, fill).unwrap();
    assert_eq!({ w.vault_state().inventory }, -45 * UNIT as i128);
    // Selling back (reducing the vault's short) is always allowed.
    w.taker_trade(0, -fill).unwrap();
    assert_eq!({ w.vault_state().inventory }, -5 * UNIT as i128);
}

#[test]
fn roll_needs_flat_portfolio_and_reduce_only_gets_there() {
    let (mut w, alice) = funded();
    w.taker_trade(0, TEN_M).unwrap(); // vault short 10 units
    w.withdraw(&alice, 1_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    expect_err(w.roll(), VaultError::NotFlat);

    // Epoch over with a request pending: the vault only reduces. A taker buy would grow the
    // short, so it fills nothing; a taker sell shrinks it and fills.
    w.taker_trade(0, TEN_M).unwrap();
    assert_eq!({ w.vault_state().inventory }, -TEN_M, "no growth in reduce-only");
    w.taker_trade(0, -2 * TEN_M).unwrap();
    assert_eq!({ w.vault_state().inventory }, 0, "fill capped at flat, never crosses");
    w.roll().unwrap();
    assert_eq!({ w.vault_state().epoch }, 2);
}

#[test]
fn matcher_approval_expires_and_anyone_can_renew_it() {
    let mut w = World::new();
    w.env.warp_to_slot(5);
    w.create_vault(InitParams { matcher_ttl_slots: 100, ..default_params(1) }).unwrap();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.advance(150, INITIAL_PRICE);
    assert!(w.taker_trade(0, TEN_M).is_err(), "expired approval refuses fills");
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
    w.taker_trade(0, TEN_M).unwrap();
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
    w.create_vault(default_params(9)).unwrap();
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
    for bad in [
        InitParams { spread_bps: 0, ..default_params(1) },
        InitParams { spread_bps: 6_000, ..default_params(1) },
        InitParams { unwind_spread_bps: 60, ..default_params(1) },
        InitParams { epoch_len_slots: 1, ..default_params(1) },
        InitParams { max_inventory_abs: 0, ..default_params(1) },
        InitParams { mode: 7, ..default_params(1) },
        InitParams { insurance_floor: 5, ..default_params(1) }, // attach mode has no floor
    ] {
        let e = w.create_vault(bad).unwrap_err();
        assert!(e.contains(&code(VaultError::InvalidParams)), "{bad:?}: {e}");
    }
    // An asset that does not exist cannot be attached.
    let e = w.create_vault(InitParams { asset_index: 40, ..default_params(1) }).unwrap_err();
    assert!(e.contains(&code(VaultError::BadPercolatorAccount)), "{e}");

    // Pre-funding the vault address does not block creation.
    let (vault, _) = state::vault_address(&pid(), &w.env.market, &w.creator.pubkey(), 1);
    w.env.svm.airdrop(&vault, 12_345).unwrap();
    w.create_vault(default_params(1)).unwrap();
    // A second creation at the same address fails.
    assert!(w.create_vault(default_params(1)).is_err());

    // Wrong collateral mint.
    let mut p = default_params(2);
    p.asset_generation_frontier = w.frontier();
    let mut ix = w.init_ix(&p);
    let fake_mint = Pubkey::new_unique();
    ix.accounts[4] = AccountMeta::new_readonly(fake_mint, false);
    let creator = w.creator.insecure_clone();
    expect_err(w.send(vec![ix], &[&creator]), VaultError::BadAccount);
}

#[test]
fn operate_only_instructions_refuse_attach_vaults() {
    let (mut w, _) = funded();
    expect_err(w.harvest(u64::MAX), VaultError::WrongMode);
    let lister = w.new_user(10_000_000);
    let pyth = w.env.set_pyth_price(&FEED, 100_000_000, -8, 0, 1_000);
    expect_err(w.list_asset(&lister, pyth), VaultError::WrongMode);
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

#[allow(unused)]
fn unused() -> Pubkey {
    system_program::ID
}
