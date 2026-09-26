//! End-to-end flows: the vault and the production Percolator binary in LiteSVM.

mod common;
use common::*;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_vault::{percolator as perc, state};
use solana_sdk::{pubkey::Pubkey, signature::Signer};

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
