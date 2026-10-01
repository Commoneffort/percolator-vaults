//! The router: every trade against a vault fills at the first Pyth price published at or after
//! a target time fixed when the request landed. These tests try to trade at a known price in
//! every way we could think of and assert each one fails.

mod common;
use common::*;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_router::{client as rclient, state as rstate, RouterError, BOND_LAMPORTS, DELAY_SECS, GRACE_SECS};
use percolator_vault::{error::VaultError, percolator as perc, processor};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

fn router_code(e: RouterError) -> String {
    format!("custom program error: {:#x}", e as u32)
}
fn vault_code(e: VaultError) -> String {
    format!("custom program error: {:#x}", e as u32)
}
fn expect(r: Result<u64, String>, code: String) {
    let msg = r.expect_err("expected the attempt to fail");
    assert!(msg.contains(&code), "expected {code}, got:\n{msg}");
}

/// A listed, funded vault (50 USDC of NAV) on the devnet-like market, rolled live.
fn live() -> World {
    let mut w = World::with_config(devnet_like_config());
    w.env.set_clock(5, 1_000);
    w.operate();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 0).unwrap();
    w
}

/// Capital + PnL of a taker's router portfolio.
fn equity(w: &World, taker: usize) -> i128 {
    let d = w.env.svm.get_account(&w.taker_portfolio(taker)).unwrap().data;
    let (capital, pnl, _) = perc::portfolio_exposure(&d).unwrap();
    capital as i128 + pnl
}

/// Moves the clock to `ts` (one slot later).
fn at(w: &mut World, ts: i64) {
    let slot = w.env.current_slot() + 1;
    w.env.set_clock(slot, ts.max(w.clock_ts()));
}

#[test]
fn a_trade_fills_at_the_first_pyth_price_after_its_target_and_nothing_else() {
    let mut w = live();
    let size = 10 * UNIT as i128;
    w.trader(0);
    let equity0 = equity(&w, 0);

    // The trader "knows" the real price is 1.10 while the mark still says 1.00, and buys.
    let id = w.request(0, size).unwrap();
    let target = w.request_target(id);
    assert_eq!(target, w.clock_ts().max(w.book().mark_publish_time) + DELAY_SECS);

    // Before the target: the mark can move forward, but the request cannot fill yet.
    at(&mut w, target - 2);
    let before = w.next_pyth(1.max(target - 2 - w.pyth_time), 1_050_000);
    w.advance_to(before).unwrap();
    w.converge();
    expect(w.fill(0, id), router_code(RouterError::NotAtTarget));

    // An update past the target that is not the first one after it cannot be used: the mark
    // never skips over the price a request fills at.
    at(&mut w, target + 3);
    let late = w.pyth_update(1_200_000, target + 3, target + 1);
    expect(w.advance_to(late), router_code(RouterError::WrongUpdate));

    // The first update at or after the target is the price, and nothing else can move the mark.
    let first = w.pyth_update(1_100_000, target + 1, target - 1);
    w.pyth_time = target + 1;
    w.price = 1_100_000;
    w.advance_to(first).unwrap();
    let newer = w.pyth_update(1_300_000, target + 2, target + 1);
    expect(w.advance_to(newer), router_code(RouterError::WrongUpdate));

    // Whoever fills it, whenever: it fills at 1.10. A stranger executes it and earns the bond.
    w.converge();
    let stranger = Keypair::new();
    w.env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let before_lamports = w.env.svm.get_balance(&stranger.pubkey()).unwrap();
    let ix = w.fill_ix(0, id, &stranger.pubkey());
    w.send(vec![ix], &[&stranger]).unwrap();
    assert_eq!(w.env.svm.get_balance(&stranger.pubkey()).unwrap() - before_lamports, BOND_LAMPORTS);
    assert_eq!({ w.vault_state().inventory }, -size);

    // Close at 1.10: the round trip is flat. Knowing 1.10 before the request earned nothing.
    w.taker_trade(0, -size).unwrap();
    let pnl = equity(&w, 0) - equity0;
    println!("trader equity change after buying on 'knowledge' of 1.10: {pnl}");
    assert!(pnl <= 0, "no profit from knowing the price in advance: {pnl}");
}

#[test]
fn filling_needs_the_effective_price_to_have_reached_the_mark() {
    let mut w = live();
    w.taker_trade(1, 5 * UNIT as i128).unwrap(); // open interest, so the per-slot cap applies
    let id = w.request(0, UNIT as i128).unwrap();
    let target = w.request_target(id);
    at(&mut w, target);
    let prev = w.pyth_time;
    w.pyth_time = target;
    w.price = INITIAL_PRICE * 103 / 100; // +3%: more than one slot of the 49 bps cap
    let u = w.pyth_update(w.price, target, prev);
    w.advance_to(u).unwrap();
    w.tick(1);
    let slot = w.env.current_slot();
    let asset = w.asset();
    w.catch_up(asset, slot, &[]);
    expect(w.fill(0, id), router_code(RouterError::NotConverged));
    w.converge();
    w.fill(0, id).unwrap();
}

#[test]
fn a_request_cannot_be_cancelled_or_withdrawn_from_and_expires_with_its_bond() {
    let mut w = live();
    let t = w.trader(0);
    let kp = w.trader_kp(0);
    let id = w.request(0, UNIT as i128).unwrap();
    let target = w.request_target(id);

    // Margin cannot leave while the request is pending.
    let dest = w.traders[&0].0.collateral;
    let ix = rclient::withdraw(&t, &dest, &w.env.vault, 1);
    expect(w.send(vec![ix], &[&kp]), router_code(RouterError::RequestPending));
    // A second request is refused too.
    expect(w.request(0, UNIT as i128).map(|_| 0), router_code(RouterError::RequestPending));

    // Nobody can expire it early (the trader included).
    let book = rstate::book_address(&percolator_router::id(), &w.vault).0;
    let book_before = w.env.svm.get_balance(&book).unwrap();
    at(&mut w, target + GRACE_SECS);
    let ix = rclient::expire(&kp.pubkey(), &w.vault, &t, id);
    expect(w.send(vec![ix.clone()], &[&kp]), router_code(RouterError::TooEarly));

    // After the grace period it expires; the bond is forfeited to the book, never to the trader.
    at(&mut w, target + GRACE_SECS + 1);
    w.send(vec![ix], &[&kp]).unwrap();
    assert_eq!(w.env.svm.get_balance(&book).unwrap() - book_before, BOND_LAMPORTS);
    assert_eq!({ w.book().len }, 0);
    // The trader is unlocked, and nothing was filled.
    assert_eq!({ w.vault_state().inventory }, 0);
    let ix = rclient::withdraw(&t, &dest, &w.env.vault, 1);
    w.send(vec![ix], &[&kp]).unwrap();
}

#[test]
fn a_request_needs_margin_for_a_stressed_price_move() {
    let mut w = live();
    let u = w.new_user(100_000_000);
    let t = rclient::TraderKeys::new(w.env.market, u.kp.pubkey());
    let mint = w.env.mint;
    let open = rclient::open_account(&t, &mint);
    let dep = rclient::deposit(&t, &u.collateral, &w.env.vault, 1_000_000); // 1 USDC
    w.send(vec![open, dep], &[&u.kp]).unwrap();
    let id = w.book().next_id;
    // 10 units at 1.00 with 10% margin needs 1 USDC, plus the stress buffer: refused.
    let ix = rclient::request(&t, &w.vault, id, 10 * UNIT as i128);
    expect(w.send(vec![ix], &[&u.kp]), router_code(RouterError::InsufficientMargin));
    let ix = rclient::request(&t, &w.vault, id, 3 * UNIT as i128);
    w.send(vec![ix], &[&u.kp]).unwrap();
}

#[test]
fn a_trade_that_only_shrinks_a_position_is_not_refused_for_margin() {
    let mut w = live();
    // A trader with 1.5 USDC of margin: enough for 6 units at 1.00 under the stressed check
    // (about 0.21 per unit), not for 9.
    let u = w.new_user(100_000_000);
    let t = rclient::TraderKeys::new(w.env.market, u.kp.pubkey());
    let mint = w.env.mint;
    let open = rclient::open_account(&t, &mint);
    let dep = rclient::deposit(&t, &u.collateral, &w.env.vault, 1_500_000);
    w.send(vec![open, dep], &[&u.kp]).unwrap();
    w.traders.insert(7, (u, t));
    let size = 6 * UNIT as i128;
    w.taker_trade(7, size).unwrap();
    // Growing or flipping the position is refused.
    expect(w.request(7, size / 2), router_code(RouterError::InsufficientMargin));
    expect(w.request(7, -(size + 9 * UNIT as i128)), router_code(RouterError::InsufficientMargin));
    // Reducing and closing are accepted and fill, although the old and the new position
    // together would not pass the check.
    w.taker_trade(7, -size / 3).unwrap();
    w.taker_trade(7, -(size - size / 3)).unwrap();
    let d = w.env.svm.get_account(&w.taker_portfolio(7)).unwrap().data;
    assert!(perc::portfolio_exposure(&d).unwrap().2.iter().all(|(_, q)| *q == 0));
}

#[test]
fn only_the_router_moves_the_mark_or_arms_a_fill() {
    let mut w = live();
    let attacker = Keypair::new();
    w.env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let seq = w.env.primary_control_sequences(w.asset() as usize);
    let mut data = vec![processor::TAG_PUSH_MARK];
    data.extend_from_slice(&2_000_000u64.to_le_bytes());
    data.extend_from_slice(&(seq.oracle_observation + 1).to_le_bytes());
    data.extend_from_slice(&seq.authority_epoch.to_le_bytes());
    let push = Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new_readonly(attacker.pubkey(), true),
            AccountMeta::new_readonly(w.vault, false),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
        ],
        data,
    };
    expect(w.send(vec![push], &[&attacker]), vault_code(VaultError::NotRouter));
    let mut data = vec![processor::TAG_ARM_FILL];
    data.extend_from_slice(&(5 * UNIT as i128).to_le_bytes());
    let arm = Instruction {
        program_id: pid(),
        accounts: vec![AccountMeta::new_readonly(attacker.pubkey(), true), AccountMeta::new(w.vault, false)],
        data,
    };
    expect(w.send(vec![arm], &[&attacker]), vault_code(VaultError::NotRouter));
    // Nor can anyone push the vault's asset mark directly on Percolator: the vault is its oracle authority.
    assert!(w.env.push_auth_mark(w.asset(), w.env.current_slot(), 2_000_000).is_err());
}

#[test]
fn forged_or_unverified_pyth_updates_are_refused() {
    let mut w = live();
    w.tick(2);
    let ts = w.clock_ts();
    let mut forged = w.pyth_update(2_000_000, ts, ts - 1);
    // Same bytes, but not owned by the Pyth receiver.
    let mut a = w.env.svm.get_account(&forged).unwrap();
    a.owner = Pubkey::new_unique();
    forged = Pubkey::new_unique();
    w.env.svm.set_account(forged, a.clone()).unwrap();
    expect(w.advance_to(forged), router_code(RouterError::BadOracle));
    // Owned by the receiver but only partially verified.
    let partial = Pubkey::new_unique();
    a.owner = percolator_vault::pyth::PYTH_RECEIVER_PROGRAM_ID;
    a.data[40] = 0;
    w.env.svm.set_account(partial, a.clone()).unwrap();
    expect(w.advance_to(partial), router_code(RouterError::BadOracle));
    // Fully verified, but for another feed.
    let other = Pubkey::new_unique();
    a.data[40] = 1;
    a.data[41..73].copy_from_slice(&[9; 32]);
    w.env.svm.set_account(other, a).unwrap();
    expect(w.advance_to(other), router_code(RouterError::BadOracle));
}

#[test]
fn trading_against_the_vault_directly_is_refused() {
    let mut w = live();
    let taker = w.env.actors[0].signer.insecure_clone();
    let pv = w.portfolio_view();
    let asset = w.asset();
    let market_id = w.env.primary_market_state().1.assets[asset as usize].market_id;
    let trade = Instruction {
        program_id: perc::PERCOLATOR_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(taker.pubkey(), true),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new(w.env.actors[0].portfolio, false),
            AccountMeta::new(w.portfolio, false),
            AccountMeta::new_readonly(pid(), false),
            AccountMeta::new(w.vault, false),
            AccountMeta::new_readonly(w.delegate, false),
        ],
        data: ProgIx::TradeCpi {
            account_a_portfolio_id: w.env.primary_portfolio_id(0),
            account_a_position_epoch: w.env.primary_portfolio_position_epoch(0),
            account_b_portfolio_id: pv.portfolio_id,
            account_b_position_epoch: pv.position_epoch,
            account_b_matcher_sequence: pv.sequence,
            asset_index: asset,
            market_id,
            size_q: 5 * UNIT as i128,
            fee_bps: 10_000,
            limit_price: 0,
            backing_fee_cap_bps: 0,
        }
        .encode(),
    };
    expect(w.send(vec![trade], &[&taker]), vault_code(VaultError::NotArmed));
    assert_eq!({ w.vault_state().inventory }, 0);
}

#[test]
fn withdrawals_only_go_to_the_owners_wallet() {
    let mut w = live();
    let t = w.trader(0);
    let kp = w.trader_kp(0);
    // Someone else's token account as destination.
    let thief_dest = Pubkey::new_unique();
    let (mint, thief) = (w.env.mint, Pubkey::new_unique());
    set_token(&mut w.env.svm, thief_dest, mint, thief, 0);
    let ix = rclient::withdraw(&t, &thief_dest, &w.env.vault, 1_000);
    expect(w.send(vec![ix], &[&kp]), router_code(RouterError::BadAccount));
    // Someone else signing for the trader's account.
    let mallory = Keypair::new();
    w.env.svm.airdrop(&mallory.pubkey(), 1_000_000_000).unwrap();
    let mut forged = rclient::TraderKeys::new(w.env.market, mallory.pubkey());
    forged.trader = t.trader;
    forged.portfolio = t.portfolio;
    forged.collateral = t.collateral;
    let mdest = Pubkey::new_unique();
    set_token(&mut w.env.svm, mdest, mint, mallory.pubkey(), 0);
    let ix = rclient::withdraw(&forged, &mdest, &w.env.vault, 1_000);
    expect(w.send(vec![ix], &[&mallory]), router_code(RouterError::BadAccount));
    // The owner can.
    let dest = w.traders[&0].0.collateral;
    let before = w.tokens(dest);
    let ix = rclient::withdraw(&t, &dest, &w.env.vault, 1_000);
    w.send(vec![ix], &[&kp]).unwrap();
    assert_eq!(w.tokens(dest) - before, 1_000);
}

#[test]
fn a_market_with_a_queued_request_cannot_be_retired() {
    let mut w = World::with_config(devnet_like_config());
    w.env.set_clock(5, 1_000);
    w.operate();
    w.env.update_market_authority_from_admin(4).unwrap();
    let admin = w.env.actors[4].signer.insecure_clone();
    let epoch0 = w.env.primary_control_sequences(0).authority_epoch;
    let ix = percolator_vault::client::accept_governance(&pid(), &admin.pubkey(), &w.env.market, epoch0);
    w.send(vec![ix], &[&admin]).unwrap();
    w.advance(EPOCH_LEN * processor::RETIRE_IDLE_EPOCHS + 1, INITIAL_PRICE);
    let retire = |w: &World| {
        let v = w.vault_state();
        let asset_epoch = w.env.primary_control_sequences(v.asset_index as usize).authority_epoch;
        let market_epoch = w.env.primary_control_sequences(0).authority_epoch;
        percolator_vault::client::retire_market(&w.keys.unwrap(), asset_epoch, market_epoch)
    };
    // Idle (no depositors), but a trader has a request queued on it.
    let id = w.request(0, UNIT as i128).unwrap();
    expect(w.send(vec![retire(&w)], &[]), vault_code(VaultError::NotIdle));
    // Once it has expired, the market can be retired.
    let target = w.request_target(id);
    at(&mut w, target + GRACE_SECS + 1);
    let (t, kp) = (w.traders[&0].1, w.trader_kp(0));
    let ix = rclient::expire(&kp.pubkey(), &w.vault, &t, id);
    w.send(vec![ix], &[&kp]).unwrap();
    w.send(vec![retire(&w)], &[]).unwrap();
}

/// The frontend and executor read an asset's control sequences at
/// `MARKET_SLOTS_OFF + asset * MARKET_ASSET_SLOT_LEN + ASSET_CONTROL_SEQUENCES_OFF`.
#[test]
fn control_sequence_offsets_match_percolator() {
    let mut w = live();
    w.taker_trade(0, UNIT as i128).unwrap(); // moves the observation sequence past its initial value
    let d = w.env.market_data(false);
    for asset in [0usize, w.asset() as usize] {
        let seq = percolator_prog::state::read_asset_control_sequences(&d, asset).unwrap();
        let base = perc::MARKET_SLOTS_OFF + asset * perc::MARKET_ASSET_SLOT_LEN + percolator_prog::constants::ASSET_CONTROL_SEQUENCES_OFF;
        let rd = |o: usize| u64::from_le_bytes(d[base + o..base + o + 8].try_into().unwrap());
        assert_eq!(rd(0), seq.oracle_observation);
        assert_eq!(rd(16), seq.authority_epoch);
    }
}
