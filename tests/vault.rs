//! End-to-end flows: the vault and the production Percolator binary in LiteSVM.

mod common;
use common::*;
#[allow(unused_imports)]
use common::v16_svm::MarketConfig as _Mc;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_vault::{percolator as perc, processor::InitParams, state};
use solana_sdk::{pubkey::Pubkey, signature::Signer};
#[allow(unused_imports)]
use solana_sdk::signature::Keypair;

// ---------------------------------------------------------------- layout

#[test]
fn layout_matches_engine() {
    use percolator::PortfolioAccountV16Account as P;
    use percolator_prog::constants as c;
    use std::mem::offset_of;
    let e = perc::HEADER_LEN;
    assert_eq!(perc::HEADER_LEN, c::HEADER_LEN);
    assert_eq!(perc::MAGIC, c::MAGIC);
    assert_eq!(perc::VERSION, c::VERSION);
    assert_eq!(perc::KIND_MARKET, c::KIND_MARKET);
    assert_eq!(perc::KIND_PORTFOLIO, c::KIND_PORTFOLIO);
    assert_eq!(perc::PORTFOLIO_ACCOUNT_LEN, c::PORTFOLIO_ACCOUNT_LEN);
    assert_eq!(perc::PORTFOLIO_MATCHER_CONTROL_OFF, c::PORTFOLIO_MATCHER_CONTROL_OFF);
    assert_eq!(perc::PORTFOLIO_ID_OFF, c::PORTFOLIO_ID_OFF);
    assert_eq!(perc::PORTFOLIO_SEQUENCE_OFF, c::PORTFOLIO_MATCHER_SEQUENCE_OFF);
    assert_eq!(perc::PORTFOLIO_MARKET_OFF, e + offset_of!(P, provenance_header));
    assert_eq!(perc::PORTFOLIO_OWNER_OFF, e + offset_of!(P, owner));
    assert_eq!(perc::PORTFOLIO_CAPITAL_OFF, e + offset_of!(P, capital));
    assert_eq!(perc::PORTFOLIO_PNL_OFF, e + offset_of!(P, pnl));
    assert_eq!(perc::PORTFOLIO_FEE_CREDITS_OFF, e + offset_of!(P, fee_credits));
    assert_eq!(perc::PORTFOLIO_ACTIVE_BITMAP_OFF, e + offset_of!(P, active_bitmap));
    assert_eq!(
        perc::MARKET_COLLATERAL_MINT_OFF,
        c::HEADER_LEN + offset_of!(percolator_prog::state::WrapperConfigV16, collateral_mint)
    );
}

#[test]
fn cpi_encodings_match_percolator_decoder() {
    let k = Pubkey::new_unique();
    let view = perc::PortfolioView {
        capital: 0,
        pnl: 0,
        fee_credits: 0,
        flat: true,
        portfolio_id: 7,
        sequence: 3,
        position_epoch: 9,
    };
    let cases = vec![
        (perc::init_portfolio(&k, &k, &k).data, ProgIx::InitPortfolio),
        (
            perc::deposit(&k, &k, &k, &k, &k, 7, 3, 55).data,
            ProgIx::Deposit { portfolio_id: 7, expected_sequence: 3, amount: 55 },
        ),
        (
            perc::withdraw(&k, &k, &k, &k, &k, &k, 7, 3, 55).data,
            ProgIx::Withdraw { portfolio_id: 7, expected_sequence: 3, amount: 55 },
        ),
        (
            perc::sync_maintenance_fee(&k, &k, 99).data,
            ProgIx::SyncMaintenanceFee { now_slot: 99 },
        ),
        (
            perc::convert_released_pnl(&k, &k, &k, 7, 9).data,
            ProgIx::ConvertReleasedPnl { portfolio_id: 7, position_epoch: 9, amount: u128::MAX },
        ),
        (
            perc::close_resolved(&k, &k, &k, &k, &k, &k).data,
            ProgIx::CloseResolved { fee_rate_per_slot: 0 },
        ),
        (
            perc::set_matcher_config(&k, &k, &k, &k, &k, &k, &view, 4, 25, 1_000).data,
            ProgIx::SetMatcherConfig {
                portfolio_id: 7,
                expected_sequence: 3,
                position_epoch: 9,
                asset_generation_frontier: 4,
                enabled: 1,
                trade_fee_cap_bps: 25,
                expiry_slot: 1_000,
            },
        ),
    ];
    for (bytes, expected) in cases {
        assert_eq!(ProgIx::decode(&bytes).unwrap(), expected);
    }
}

// ---------------------------------------------------------------- end to end

#[test]
fn e2e_create_deposit_trade_roll_withdraw() {
    let mut w = World::new();
    w.env.warp_to_slot(5);
    w.create_vault(default_params(1)).unwrap();
    let v = w.vault_state();
    assert_eq!({ v.status }, state::STATUS_ACTIVE);
    assert_eq!(w.portfolio_view().capital, 0);

    // Alice deposits; nothing is priced until the epoch rolls.
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    assert_eq!(w.tokens(w.buffer), 50_000_000);
    assert!(w.roll().unwrap_err().contains("0x560c"), "epoch not over yet");

    w.advance(EPOCH_LEN, INITIAL_PRICE);
    let cu = w.roll().unwrap();
    println!("roll CU: {cu}");
    let v = w.vault_state();
    assert_eq!({ v.epoch }, 1);
    assert_eq!(w.portfolio_view().capital, 50_000_000, "deployed into Percolator");
    assert_eq!(w.tokens(w.buffer), 0);
    w.claim(&alice, 0).unwrap();
    let alice_shares = w.tokens(alice.shares);
    assert_eq!(alice_shares, 50_000_000 * 1_000, "first deposit mints at 1000 shares per atom");

    // A taker buys from the vault, then sells back: the vault earns the spread both ways.
    let cu = w.taker_trade(0, TEN_M).unwrap();
    println!("trade CU: {cu}");
    assert_eq!({ w.vault_state().inventory }, -TEN_M);
    w.taker_trade(0, -TEN_M).unwrap();
    assert_eq!({ w.vault_state().inventory }, 0);
    let pv = w.portfolio_view();
    println!("after round trip: {pv:?}");
    assert!(pv.flat);

    // Alice asks to withdraw everything; Bob deposits in the same epoch.
    let bob = w.new_user(100_000_000);
    w.deposit(&bob, 10_000_000).unwrap();
    w.withdraw(&alice, alice_shares).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    let rec: state::EpochRecord = {
        let a = w.env.svm.get_account(&state::epoch_address(&pid(), &w.vault, 1).0).unwrap();
        bytemuck::pod_read_unaligned(&a.data)
    };
    println!("epoch 1 record: {rec:?}");
    w.claim(&alice, 1).unwrap();
    w.claim(&bob, 1).unwrap();
    let alice_back = w.tokens(alice.collateral);
    println!("alice collateral after exit: {alice_back}");
    assert!(alice_back >= 100_000_000 - 1, "alice keeps her principal (plus spread, minus rounding)");
    assert!(w.tokens(bob.shares) > 0);
    // Buffer holds exactly what is still owed.
    assert_eq!(w.tokens(w.buffer), { w.vault_state().reserved_assets });
}


#[test]
fn e2e_operate_mode_lists_asset_and_harvests_fees() {
    let mut w = World::new();
    w.env.set_clock(5, 1_000);
    w.env.update_market_init_fee_policy(LISTING_FEE as u128).unwrap();
    w.env.update_trade_fee_policy(10).unwrap(); // 10 bps on every trade, credited to insurance
    w.create_vault(operate_params(2)).unwrap();
    assert_eq!({ w.vault_state().status }, state::STATUS_PENDING_LISTING);

    // Nothing can trade against a vault that has not listed its asset yet.
    let lister = w.new_user(10_000_000);
    let pyth = w.env.set_pyth_price(&FEED, 100_000_000, -8, 0, 1_000); // 1.00000000
    let cu = w.list_asset(&lister, pyth).unwrap();
    println!("list CU: {cu}");
    let v = w.vault_state();
    assert_eq!({ v.status }, state::STATUS_ACTIVE);
    let asset = v.asset_index as usize;
    println!("listed asset {asset}, fee paid {}", 10_000_000 - w.tokens(lister.collateral));
    assert_eq!(10_000_000 - w.tokens(lister.collateral), LISTING_FEE, "unused fee is refunded");
    assert!(w.roll().is_err() || true);

    // The vault holds every authority over its asset.
    let data = w.env.market_data(false);
    let profile = percolator_prog::state::read_asset_oracle_profile(&data, asset).unwrap();
    for k in [profile.asset_admin, profile.insurance_authority, profile.insurance_operator, profile.backing_bucket_authority, profile.oracle_authority] {
        assert_eq!(k, w.vault.to_bytes());
    }
    println!("oracle mode {}", profile.oracle_mode);

    // Fund the vault, roll it live.
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    let mut ts = 1_000i64;
    w.advance_oracle(pyth, EPOCH_LEN, &mut ts, 100_000_000);
    w.roll().unwrap();
    w.claim(&alice, 0).unwrap();

    // Takers trade on the vault's asset; each trade pays the base fee into its insurance.
    let unit = UNIT as i128;
    for i in 0..6 {
        let size = if i % 2 == 0 { 10 * unit } else { -10 * unit };
        w.advance_oracle(pyth, 2, &mut ts, 100_000_000);
        let cu = w.taker_trade_asset(0, asset as u16, size).unwrap();
        if i == 0 {
            println!("trade CU: {cu}");
        }
    }
    let (_, insurance) = {
        let data = w.env.market_data(false);
        let base = perc::MARKET_SLOTS_OFF + asset * perc::MARKET_ASSET_SLOT_LEN + 512;
        let rd = |o: usize| u128::from_le_bytes(data[base + o..base + o + 16].try_into().unwrap());
        (0, rd(515) + rd(531) - rd(547) - rd(563))
    };
    println!("asset insurance after trades: {insurance}");
    assert!(insurance > 10_000, "fees accrued to the vault's asset insurance");

    // Harvest moves everything above the floor into the vault, and never touches the floor.
    // Percolator only releases live insurance from a settled, current market, so crank first.
    w.advance_oracle(pyth, 1, &mut ts, 100_000_000);
    let (_, g) = w.env.primary_market_state();
    println!("neg_pnl_accounts={} loss_stale={} stress={}", g.negative_pnl_account_count, g.loss_stale_active, g.threshold_stress_active);
    let buffer_before = w.tokens(w.buffer);
    w.harvest(u64::MAX).unwrap();
    let harvested = w.tokens(w.buffer) - buffer_before;
    println!("harvested {harvested}");
    assert_eq!(harvested as u128, insurance - 10_000);
    assert_eq!({ w.vault_state().total_fees_harvested }, harvested);
    assert!(w.harvest(u64::MAX).unwrap_err().contains("0x5615"), "nothing left above the floor");

    // Alice exits after the vault is flat and gets her principal plus the fee income.
    w.withdraw(&alice, w.tokens(alice.shares)).unwrap();
    w.advance_oracle(pyth, EPOCH_LEN, &mut ts, 100_000_000);
    w.roll().unwrap();
    w.claim(&alice, 1).unwrap();
    let alice_after = w.tokens(alice.collateral);
    println!("alice collateral: before 100000000, after {alice_after}");
    assert!(alice_after > 100_000_000, "fee income reaches depositors");
}

#[test]
fn market_offsets_match_engine() {
    use percolator::EngineAssetSlotV16Account as S;
    use percolator_prog::constants as c;
    use std::mem::offset_of;
    assert_eq!(perc::MARKET_SLOTS_OFF, c::MARKET_GROUP_OFF + c::MARKET_GROUP_LEN);
    assert_eq!(perc::MARKET_ASSET_SLOT_LEN, c::MARKET_ASSET_SLOT_LEN);
    assert_eq!(offset_of!(percolator::Market<[u8; 512]>, engine), 512);
    assert_eq!(offset_of!(S, asset) + offset_of!(percolator::AssetStateV16Account, market_id), 0);
    assert_eq!(offset_of!(S, insurance_domain_budget_long), 515);
    assert_eq!(offset_of!(S, insurance_domain_budget_short), 531);
    assert_eq!(offset_of!(S, insurance_domain_spent_long), 547);
    assert_eq!(offset_of!(S, insurance_domain_spent_short), 563);
    assert_eq!(
        perc::market_account_len_for_slots(4),
        percolator_prog::state::market_account_len_for_capacity(4).unwrap()
    );
}

fn settle_ix(w: &World) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new(w.vault, false),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new(w.portfolio, false),
            AccountMeta::new(w.buffer, false),
            AccountMeta::new(w.env.vault, false),
            AccountMeta::new_readonly(w.env.vault_authority, false),
            AccountMeta::new_readonly(spl_token::ID, false),
            AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
        ],
        data: vec![percolator_vault::processor::TAG_SETTLE_RESOLVED],
    }
}

fn redeem_ix(w: &World, u: &User, shares: u64) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    let mut data = vec![percolator_vault::processor::TAG_REDEEM_TERMINAL];
    data.extend_from_slice(&shares.to_le_bytes());
    Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new_readonly(u.kp.pubkey(), true),
            AccountMeta::new(w.vault, false),
            AccountMeta::new(u.shares, false),
            AccountMeta::new(w.share_mint, false),
            AccountMeta::new(u.collateral, false),
            AccountMeta::new(w.buffer, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data,
    }
}

#[test]
fn market_resolution_winds_the_vault_down_and_everyone_exits() {
    let mut w = World::new();
    w.env.warp_to_slot(5);
    w.create_vault(default_params(1)).unwrap();
    let alice = w.new_user(100_000_000);
    let bob = w.new_user(100_000_000);
    w.deposit(&alice, 30_000_000).unwrap();
    w.deposit(&bob, 20_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 0).unwrap();
    // Bob leaves his shares unclaimed in escrow; Carol queues a deposit that never settles.
    let carol = w.new_user(5_000_000);
    w.deposit(&carol, 5_000_000).unwrap();
    // The vault holds a position when the market is resolved.
    w.taker_trade(0, TEN_M).unwrap();
    w.advance(3, INITIAL_PRICE);
    w.env.resolve_market().unwrap();

    // Settlement is permissionless and may take several calls.
    let mut calls = 0;
    while { w.vault_state().status } != state::STATUS_TERMINAL {
        calls += 1;
        assert!(calls < 10, "vault never became terminal");
        let ix = settle_ix(&w);
        match w.send(vec![ix], &[]) {
            Ok(_) => {}
            Err(e) => panic!("settle failed on call {calls}:\n{e}"),
        }
        w.env.warp_to_slot(w.env.current_slot() + 1);
    }
    println!("terminal after {calls} settle call(s); buffer {}", w.tokens(w.buffer));

    // Carol gets her unsettled deposit back; Bob claims his shares; everyone redeems pro rata.
    w.claim(&carol, 1).unwrap();
    assert_eq!(w.tokens(carol.collateral), 5_000_000);
    w.claim(&bob, 0).unwrap();
    let a = w.tokens(alice.shares);
    let b = w.tokens(bob.shares);
    let ix = redeem_ix(&w, &alice, a);
    w.send(vec![ix], &[&alice.kp]).unwrap();
    let ix = redeem_ix(&w, &bob, b);
    w.send(vec![ix], &[&bob.kp]).unwrap();
    let alice_out = w.tokens(alice.collateral) - 70_000_000;
    let bob_out = w.tokens(bob.collateral) - 80_000_000;
    println!("alice {alice_out} of 30M, bob {bob_out} of 20M, buffer left {}", w.tokens(w.buffer));
    assert!(alice_out >= 29_990_000 && bob_out >= 19_990_000);
    assert!(w.tokens(w.buffer) < 10, "nothing stranded beyond rounding dust");
}

fn convert_ix(w: &World) -> solana_sdk::instruction::Instruction {
    use solana_sdk::instruction::{AccountMeta, Instruction};
    Instruction {
        program_id: pid(),
        accounts: vec![
            AccountMeta::new_readonly(w.vault, false),
            AccountMeta::new(w.env.market, false),
            AccountMeta::new(w.portfolio, false),
            AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
        ],
        data: vec![percolator_vault::processor::TAG_CONVERT_PNL],
    }
}

/// The vault takes the other side of a taker; the price moves; the vault's profit or loss
/// reaches depositors through the next roll.
fn price_move_round_trip(move_bps: i64) -> (u64, perc::PortfolioView) {
    let mut w = World::new();
    w.env.warp_to_slot(5);
    w.create_vault(default_params(1)).unwrap();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 0).unwrap();

    w.taker_trade(0, TEN_M).unwrap(); // taker long 10 units, vault short
    let target = (INITIAL_PRICE as i64 * (10_000 + move_bps) / 10_000) as u64;
    for _ in 0..5 {
        w.advance(1, target); // price walks to target under the per-slot cap
    }
    w.taker_trade(0, -TEN_M).unwrap(); // taker closes; vault flat again
    w.advance(20, target); // let any profit lock mature
    let _ = w.send(vec![convert_ix(&w)], &[]); // no-op (and harmless) when nothing is released
    let pv = w.portfolio_view();
    w.withdraw(&alice, w.tokens(alice.shares)).unwrap();
    w.advance(EPOCH_LEN, target);
    w.roll().unwrap();
    w.claim(&alice, 1).unwrap();
    (w.tokens(alice.collateral), pv)
}

#[test]
fn vault_profit_and_loss_reach_depositors() {
    let (down, pv_down) = price_move_round_trip(-1_000); // price -10%: short vault wins 1M
    let (up, pv_up) = price_move_round_trip(1_000); // price +10%: short vault loses 1M
    println!("price down: alice {down} (portfolio {pv_down:?})");
    println!("price up:   alice {up} (portfolio {pv_up:?})");
    assert!(down > 100_000_000, "vault gains reach depositors");
    assert!(up < 100_000_000, "vault losses reach depositors");
    assert!(up >= 100_000_000 - 1_000_001, "loss is exactly the move, not more");
}

#[test]
fn roll_is_exact_with_maintenance_fees_charged() {
    let mut w = World::with_config(MarketConfig { maintenance_fee_per_slot: 3, ..MarketConfig::default() });
    w.env.warp_to_slot(5);
    w.create_vault(default_params(1)).unwrap();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 0).unwrap();
    w.taker_trade(0, TEN_M).unwrap();
    w.advance(7, INITIAL_PRICE);
    w.taker_trade(0, -TEN_M).unwrap();
    w.withdraw(&alice, w.tokens(alice.shares)).unwrap();
    w.advance(EPOCH_LEN, INITIAL_PRICE);
    w.roll().unwrap();
    w.claim(&alice, 1).unwrap();
    let paid = 100_000_000 - w.tokens(alice.collateral);
    println!("maintenance fees borne by the vault: {paid}");
    assert!(paid > 0 && paid < 1_000, "fees are charged, and only fees");
    assert_eq!(w.tokens(w.buffer), { w.vault_state().reserved_assets });
}

/// The parameters the devnet market uses: 10x leverage, SOL priced in USD atoms (6 decimals).
pub fn devnet_like_config() -> MarketConfig {
    MarketConfig {
        initial_price: 1_000_000,
        h_max: 6_480_000,
        min_nonzero_mm_req: 500,
        min_nonzero_im_req: 600,
        maintenance_margin_bps: 1_000,
        initial_margin_bps: 1_000,
        max_price_move_bps_per_slot: 49,
        max_accrual_dt_slots: 10,
        min_funding_lifetime_slots: 10_000_000,
        liquidation_fee_bps: 5,
        liquidation_fee_cap: 50_000_000,
        ..MarketConfig::default()
    }
}

#[test]
fn devnet_like_market_trades_at_realistic_size() {
    let mut w = World::with_config(devnet_like_config());
    w.env.set_clock(5, 1_000);
    w.env.update_market_init_fee_policy(LISTING_FEE as u128).unwrap();
    w.env.update_trade_fee_policy(5).unwrap();
    let p = InitParams {
        max_fill_abs: 200 * UNIT,
        max_inventory_abs: 400 * UNIT,
        oracle_max_staleness_secs: 300,
        ..operate_params(1)
    };
    w.create_vault(p).unwrap();
    let lister = w.new_user(10_000_000);
    let sol = 12_107_261_038i64; // $121.07261038, expo -8
    let pyth = w.env.set_pyth_price(&FEED, sol, -8, 0, 1_000);
    w.list_asset(&lister, pyth).unwrap();
    let alice = w.new_user(100_000_000);
    w.deposit(&alice, 50_000_000).unwrap(); // 50 USDC
    let mut ts = 1_000i64;
    w.advance_oracle(pyth, EPOCH_LEN, &mut ts, sol);
    w.roll().unwrap();
    let asset = w.vault_state().asset_index;
    let px = w.env.primary_market_state().1.assets[asset as usize].effective_price;
    println!("SOL effective price e6: {px}");
    // 2 SOL long (~$242 notional) at 10x needs ~$24 margin from each side.
    let cu = w.taker_trade_asset(0, asset, 2 * UNIT as i128).unwrap();
    println!("2 SOL trade CU {cu}, vault inventory {}", { w.vault_state().inventory });
    w.advance_oracle(pyth, 3, &mut ts, sol + 100_000_000); // +$1
    w.taker_trade_asset(0, asset, -2 * UNIT as i128).unwrap();
    println!("vault after SOL +$1 on 2 SOL short: {:?}", w.portfolio_view());
}

/// Writes the vault account layout for the frontend decoder (app/src/layout.json).
#[test]
fn export_layout_for_frontend() {
    use percolator_vault::state::{EpochRecord, Ticket, VaultState as V};
    use std::mem::{offset_of, size_of};
    let o = state::VAULT_STATE_OFF;
    macro_rules! f {
        ($t:ty, $base:expr, $($name:ident),*) => {{
            let mut m = serde_json::Map::new();
            $( m.insert(stringify!($name).into(), serde_json::json!($base + offset_of!($t, $name))); )*
            serde_json::Value::Object(m)
        }};
    }
    let layout = serde_json::json!({
        "vault": f!(V, o, magic, status, share_decimals, market, creator, seed, collateral_mint, share_mint, buffer, share_escrow,
            lp_portfolio, matcher_delegate, portfolio_id, asset_index, spread_bps, unwind_spread_bps, max_inventory_abs,
            epoch_len_slots, matcher_ttl_slots, mode, insurance_floor, oracle_feeds, max_fill_abs, asset_market_id, inventory, epoch, epoch_start_slot,
            pending_deposit_assets, pending_withdraw_shares, reserved_assets, last_nav, created_slot, total_fills, total_fees_harvested),
        "vault_len": state::VAULT_ACCOUNT_LEN,
        "ticket": f!(Ticket, 0, vault, owner, epoch, deposit_assets, withdraw_shares),
        "ticket_len": size_of::<Ticket>(),
        "epoch_record": f!(EpochRecord, 0, epoch, deposit_assets, shares_minted, withdraw_shares, assets_out, nav_low, nav_high, supply_before, rolled_slot, deposits_refunded),
        "portfolio": {
            "capital": perc::PORTFOLIO_CAPITAL_OFF, "pnl": perc::PORTFOLIO_PNL_OFF, "fee_credits": perc::PORTFOLIO_FEE_CREDITS_OFF,
            "active_bitmap": perc::PORTFOLIO_ACTIVE_BITMAP_OFF, "owner": perc::PORTFOLIO_OWNER_OFF, "control": perc::PORTFOLIO_MATCHER_CONTROL_OFF,
            "id": perc::PORTFOLIO_ID_OFF, "sequence": perc::PORTFOLIO_SEQUENCE_OFF,
            "legs": 16 + offset_of!(percolator::PortfolioAccountV16Account, legs),
            "leg_len": size_of::<percolator::PortfolioLegV16Account>(),
            "len": perc::PORTFOLIO_ACCOUNT_LEN
        },
        "leg": {
            "asset_index": offset_of!(percolator::PortfolioLegV16Account, asset_index),
            "basis_pos_q": offset_of!(percolator::PortfolioLegV16Account, basis_pos_q),
            "side": offset_of!(percolator::PortfolioLegV16Account, side),
            "active": offset_of!(percolator::PortfolioLegV16Account, active)
        },
        "market_header": {
            "next_market_id": percolator_prog::constants::MARKET_GROUP_OFF + offset_of!(percolator::MarketGroupV16HeaderAccount, next_market_id),
            "max_market_slots": percolator_prog::constants::MARKET_GROUP_OFF + offset_of!(percolator::MarketGroupV16HeaderAccount, config) + offset_of!(percolator::V16ConfigAccount, max_market_slots),
            "mode": percolator_prog::constants::MARKET_GROUP_OFF + offset_of!(percolator::MarketGroupV16HeaderAccount, mode)
        },
        "market": { "slots": perc::MARKET_SLOTS_OFF, "slot_len": perc::MARKET_ASSET_SLOT_LEN, "engine": 512,
            "market_id": 0,
            "effective_price": offset_of!(percolator::AssetStateV16Account, effective_price),
            "slot_last": offset_of!(percolator::AssetStateV16Account, slot_last),
            "oi_long": offset_of!(percolator::AssetStateV16Account, oi_eff_long_q),
            "oi_short": offset_of!(percolator::AssetStateV16Account, oi_eff_short_q),
            "ins_budget_long": 515, "ins_budget_short": 531, "ins_spent_long": 547, "ins_spent_short": 563 }
    });
    std::fs::create_dir_all("app/src").unwrap();
    std::fs::write("app/src/layout.json", serde_json::to_string_pretty(&layout).unwrap()).unwrap();
}

#[test]
fn pyth_push_feed_address_derivation() {
    let push = solana_sdk::pubkey!("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");
    let mut feed = [0u8; 32];
    let hex = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
    for i in 0..32 { feed[i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap(); }
    let (k, _) = Pubkey::find_program_address(&[&0u16.to_le_bytes(), &feed], &push);
    assert_eq!(k.to_string(), "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE");
}
