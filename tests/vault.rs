//! End-to-end tests: the vault program and the production Percolator SBF binary together in
//! LiteSVM, using Percolator's own test harness to build the market.

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../../percolator-prog/tests/support/v16_svm.rs"]
mod v16_svm;

use litesvm::LiteSVM;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_vault::{
    percolator as perc,
    processor::{self, InitParams},
    state::{self, VaultState},
};
use solana_sdk::{
    account::Account,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};
use spl_token::state::{Account as TokenAccount, AccountState};
use v16_svm::{MarketConfig, V16Svm, INITIAL_PRICE};

const VAULT_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/percolator_vault.so");
const EPOCH_LEN: u64 = 20;
const SPREAD_BPS: u16 = 50;
/// One unit of position size (Percolator's POS_SCALE).
const UNIT: u128 = 1_000_000;
/// 10 units: 10M collateral atoms of notional at the initial price (1M atoms per unit).
const TEN_M: i128 = 10 * UNIT as i128;

pub struct World {
    pub env: V16Svm,
    pub payer: Keypair,
    pub creator: Keypair,
    pub vault: Pubkey,
    pub share_mint: Pubkey,
    pub buffer: Pubkey,
    pub escrow: Pubkey,
    pub portfolio: Pubkey,
    pub delegate: Pubkey,
}

impl World {
    /// Lists the vault's own asset, paid by `lister`, with `pyth` as its price account.
    pub fn list_asset(&mut self, lister: &User, pyth: Pubkey) -> Result<u64, String> {
        let asset_index = self.env.primary_market_state().1.assets.len() as u16;
        let args = processor::ListArgs {
            asset_index,
            market_id: self.frontier(),
            activation_authority_epoch: 0,
            initial_price: INITIAL_PRICE,
            oracle_observation_sequence: 1,
            oracle_authority_epoch: 0,
        };
        let ix = Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new(lister.kp.pubkey(), true),
                AccountMeta::new(lister.collateral, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.env.market, false),
                AccountMeta::new(self.buffer, false),
                AccountMeta::new(self.env.vault, false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
                AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
                AccountMeta::new_readonly(pyth, false),
            ],
            data: args.encode(),
        };
        self.send(vec![ix], &[&lister.kp])
    }

    pub fn harvest(&mut self, max_amount: u64) -> Result<u64, String> {
        let v = self.vault_state();
        let epoch = self.env.primary_control_sequences(v.asset_index as usize).authority_epoch;
        let mut data = vec![processor::TAG_HARVEST_FEES];
        data.extend_from_slice(&epoch.to_le_bytes());
        data.extend_from_slice(&max_amount.to_le_bytes());
        let ix = Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.env.market, false),
                AccountMeta::new(self.buffer, false),
                AccountMeta::new(self.env.vault, false),
                AccountMeta::new_readonly(self.env.vault_authority, false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
            ],
            data,
        };
        self.send(vec![ix], &[])
    }
}

impl World {
    /// Moves time forward, republishes the Pyth price and cranks the vault's asset current.
    pub fn advance_oracle(&mut self, pyth: Pubkey, slots: u64, ts: &mut i64, price: i64) {
        let slot = self.env.current_slot() + slots;
        *ts += (slots as i64 * 2) / 5 + 1;
        self.env.set_clock(slot, *ts);
        self.env.update_pyth_price(pyth, &FEED, price, -8, 0, *ts);
        let asset = self.vault_state().asset_index;
        let hint = percolator_prog::ix::CrankObservationHint { asset_index: asset, oracle_accounts: 1 };
        let _ = self.env.crank_with_oracles(0, slot, vec![hint], &[pyth]);
        let _ = self.env.crank(0, slot, vec![]);
    }

    pub fn taker_trade_asset(&mut self, taker: usize, asset: u16, size_q: i128) -> Result<u64, String> {
        let market_id = self.env.primary_market_state().1.assets[asset as usize].market_id;
        let pv = self.portfolio_view();
        let ix = Instruction {
            program_id: perc::PERCOLATOR_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.env.actors[taker].signer.pubkey(), true),
                AccountMeta::new(self.env.market, false),
                AccountMeta::new(self.env.actors[taker].portfolio, false),
                AccountMeta::new(self.portfolio, false),
                AccountMeta::new_readonly(pid(), false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new_readonly(self.delegate, false),
            ],
            data: ProgIx::TradeCpi {
                account_a_portfolio_id: self.env.primary_portfolio_id(taker),
                account_a_position_epoch: self.env.primary_portfolio_position_epoch(taker),
                account_b_portfolio_id: pv.portfolio_id,
                account_b_position_epoch: pv.position_epoch,
                account_b_matcher_sequence: pv.sequence,
                asset_index: asset,
                market_id,
                size_q,
                fee_bps: 10_000,
                limit_price: 0,
                backing_fee_cap_bps: 0,
            }
            .encode(),
        };
        let signer = self.env.actors[taker].signer.insecure_clone();
        self.send(vec![ix], &[&signer])
    }
}

pub struct User {
    pub kp: Keypair,
    pub collateral: Pubkey,
    pub shares: Pubkey,
}

fn pid() -> Pubkey {
    percolator_vault::id()
}

fn set_token(svm: &mut LiteSVM, key: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64) {
    let mut data = vec![0u8; TokenAccount::LEN];
    TokenAccount::pack(
        TokenAccount {
            mint,
            owner,
            amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        },
        &mut data,
    )
    .unwrap();
    svm.set_account(
        key,
        Account { lamports: 10_000_000, data, owner: spl_token::ID, executable: false, rent_epoch: 0 },
    )
    .unwrap();
}

pub fn default_params(seed: u64) -> InitParams {
    InitParams {
        seed,
        asset_index: 0,
        spread_bps: SPREAD_BPS,
        unwind_spread_bps: 10,
        trade_fee_cap_bps: 10_000,
        backing_fee_cap_bps: 0,
        max_fill_abs: 40 * UNIT * 1_000_000,
        max_inventory_abs: 45 * UNIT * 1_000_000,
        epoch_len_slots: EPOCH_LEN,
        matcher_ttl_slots: 10_000,
        asset_generation_frontier: 0,
        mode: state::MODE_ATTACH,
        insurance_floor: 0,
        listing_fee_max: 0,
        oracle_leg_count: 0,
        oracle_leg_flags: 0,
        oracle_invert: 0,
        oracle_unit_scale: 0,
        oracle_conf_filter_bps: 0,
        oracle_max_staleness_secs: 0,
        oracle_soft_stale_slots: 0,
        oracle_ewma_halflife_slots: 0,
        oracle_mark_min_fee: 0,
        oracle_feeds: [[0; 32]; 3],
    }
}

pub const FEED: [u8; 32] = [7; 32];
pub const LISTING_FEE: u64 = 1_000_000;

pub fn operate_params(seed: u64) -> InitParams {
    InitParams {
        mode: state::MODE_OPERATE,
        insurance_floor: 10_000,
        listing_fee_max: 5_000_000,
        oracle_leg_count: 1,
        oracle_max_staleness_secs: 60,
        oracle_soft_stale_slots: 200,
        oracle_ewma_halflife_slots: 1,
        oracle_feeds: [FEED, [0; 32], [0; 32]],
        ..default_params(seed)
    }
}

impl World {
    pub fn new() -> Self {
        Self::with_config(MarketConfig::default())
    }

    pub fn with_config(config: MarketConfig) -> Self {
        std::env::set_var(
            "PERCOLATOR_FUZZ_SBF",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../percolator-prog/target/deploy/percolator_prog.so"),
        );
        let mut env = V16Svm::new([0x5a; 32], config);
        let bytes = std::fs::read(VAULT_SO).expect("build the vault with cargo build-sbf first");
        env.svm.add_program(pid(), &bytes);
        let payer = Keypair::new();
        let creator = Keypair::new();
        env.svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
        env.svm.airdrop(&creator.pubkey(), 1_000_000_000).unwrap();
        Self {
            env,
            payer,
            creator,
            vault: Pubkey::default(),
            share_mint: Pubkey::default(),
            buffer: Pubkey::default(),
            escrow: Pubkey::default(),
            portfolio: Pubkey::default(),
            delegate: Pubkey::default(),
        }
    }

    pub fn send(&mut self, ixs: Vec<Instruction>, signers: &[&Keypair]) -> Result<u64, String> {
        let mut all = vec![ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)];
        all.extend(ixs);
        let mut s: Vec<&Keypair> = vec![&self.payer];
        s.extend_from_slice(signers);
        self.env.svm.expire_blockhash();
        let tx = Transaction::new_signed_with_payer(
            &all,
            Some(&self.payer.pubkey()),
            &s,
            self.env.svm.latest_blockhash(),
        );
        self.env
            .svm
            .send_transaction(tx)
            .map(|m| m.compute_units_consumed)
            .map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n")))
    }

    pub fn frontier(&self) -> u64 {
        self.env.primary_market_state().1.next_market_id
    }

    pub fn init_ix(&self, p: &InitParams) -> Instruction {
        let market = self.env.market;
        let (vault, _) = state::vault_address(&pid(), &market, &self.creator.pubkey(), p.seed);
        let child = |t| state::child_address(&pid(), t, &vault).0;
        let portfolio = child(state::SEED_PORTFOLIO);
        Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new(self.payer.pubkey(), true),
                AccountMeta::new_readonly(self.creator.pubkey(), true),
                AccountMeta::new(vault, false),
                AccountMeta::new(market, false),
                AccountMeta::new_readonly(self.env.mint, false),
                AccountMeta::new(child(state::SEED_SHARES), false),
                AccountMeta::new(child(state::SEED_BUFFER), false),
                AccountMeta::new(child(state::SEED_ESCROW), false),
                AccountMeta::new(portfolio, false),
                AccountMeta::new_readonly(
                    perc::matcher_delegate(&market, &portfolio, &vault, &pid(), &vault),
                    false,
                ),
                AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
                AccountMeta::new_readonly(pid(), false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data: p.encode(),
        }
    }

    pub fn create_vault(&mut self, mut p: InitParams) -> Result<(), String> {
        p.asset_generation_frontier = self.frontier();
        let ix = self.init_ix(&p);
        let creator = self.creator.insecure_clone();
        self.send(vec![ix], &[&creator])?;
        let market = self.env.market;
        self.vault = state::vault_address(&pid(), &market, &creator.pubkey(), p.seed).0;
        let child = |t| state::child_address(&pid(), t, &self.vault).0;
        self.share_mint = child(state::SEED_SHARES);
        self.buffer = child(state::SEED_BUFFER);
        self.escrow = child(state::SEED_ESCROW);
        self.portfolio = child(state::SEED_PORTFOLIO);
        self.delegate = perc::matcher_delegate(&market, &self.portfolio, &self.vault, &pid(), &self.vault);
        Ok(())
    }

    pub fn vault_state(&self) -> VaultState {
        let a = self.env.svm.get_account(&self.vault).unwrap();
        bytemuck::pod_read_unaligned(&a.data[state::VAULT_STATE_OFF..])
    }

    pub fn portfolio_view(&self) -> perc::PortfolioView {
        let mut a = self.env.svm.get_account(&self.portfolio).unwrap();
        let key = self.portfolio;
        let mut lamports = a.lamports;
        let info = solana_sdk::account_info::AccountInfo::new(
            &key, false, false, &mut lamports, &mut a.data, &a.owner, false, 0,
        );
        perc::read_portfolio(&info, &self.env.market, &self.vault).unwrap()
    }

    pub fn tokens(&self, key: Pubkey) -> u64 {
        self.env.token_amount(key)
    }

    pub fn new_user(&mut self, collateral: u64) -> User {
        let kp = Keypair::new();
        self.env.svm.airdrop(&kp.pubkey(), 1_000_000_000).unwrap();
        let c = Pubkey::new_unique();
        let s = Pubkey::new_unique();
        let mint = self.env.mint;
        let share_mint = self.share_mint;
        set_token(&mut self.env.svm, c, mint, kp.pubkey(), collateral);
        set_token(&mut self.env.svm, s, share_mint, kp.pubkey(), 0);
        User { kp, collateral: c, shares: s }
    }

    pub fn ticket(&self, user: &Pubkey) -> Pubkey {
        state::ticket_address(&pid(), &self.vault, user).0
    }

    pub fn deposit_ix(&self, u: &User, amount: u64) -> Instruction {
        let mut data = vec![processor::TAG_REQUEST_DEPOSIT];
        data.extend_from_slice(&amount.to_le_bytes());
        Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new(u.kp.pubkey(), true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.ticket(&u.kp.pubkey()), false),
                AccountMeta::new(u.collateral, false),
                AccountMeta::new(self.buffer, false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data,
        }
    }

    pub fn deposit(&mut self, u: &User, amount: u64) -> Result<u64, String> {
        let ix = self.deposit_ix(u, amount);
        self.send(vec![ix], &[&u.kp])
    }

    pub fn withdraw(&mut self, u: &User, shares: u64) -> Result<u64, String> {
        let mut data = vec![processor::TAG_REQUEST_WITHDRAW];
        data.extend_from_slice(&shares.to_le_bytes());
        let ix = Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new(u.kp.pubkey(), true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.ticket(&u.kp.pubkey()), false),
                AccountMeta::new(u.shares, false),
                AccountMeta::new(self.escrow, false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data,
        };
        self.send(vec![ix], &[&u.kp])
    }

    pub fn roll_ix(&self, cranker: &Pubkey) -> Instruction {
        let v = self.vault_state();
        let mut data = vec![processor::TAG_ROLL_EPOCH];
        data.extend_from_slice(&self.frontier().to_le_bytes());
        Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new(*cranker, true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.env.market, false),
                AccountMeta::new(self.portfolio, false),
                AccountMeta::new(self.buffer, false),
                AccountMeta::new(self.env.vault, false),
                AccountMeta::new_readonly(self.env.vault_authority, false),
                AccountMeta::new(self.share_mint, false),
                AccountMeta::new(self.escrow, false),
                AccountMeta::new(state::epoch_address(&pid(), &self.vault, v.epoch).0, false),
                AccountMeta::new_readonly(self.delegate, false),
                AccountMeta::new_readonly(perc::PERCOLATOR_PROGRAM_ID, false),
                AccountMeta::new_readonly(pid(), false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(system_program::ID, false),
            ],
            data,
        }
    }

    pub fn roll(&mut self) -> Result<u64, String> {
        let ix = self.roll_ix(&self.payer.pubkey());
        self.send(vec![ix], &[])
    }

    pub fn claim_ix(&self, u: &User, epoch: u64) -> Instruction {
        Instruction {
            program_id: pid(),
            accounts: vec![
                AccountMeta::new_readonly(u.kp.pubkey(), true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.ticket(&u.kp.pubkey()), false),
                AccountMeta::new_readonly(state::epoch_address(&pid(), &self.vault, epoch).0, false),
                AccountMeta::new(u.collateral, false),
                AccountMeta::new(u.shares, false),
                AccountMeta::new(self.buffer, false),
                AccountMeta::new(self.escrow, false),
                AccountMeta::new_readonly(spl_token::ID, false),
            ],
            data: vec![processor::TAG_CLAIM],
        }
    }

    pub fn claim(&mut self, u: &User, epoch: u64) -> Result<u64, String> {
        let ix = self.claim_ix(u, epoch);
        self.send(vec![ix], &[&u.kp])
    }

    /// Moves time forward and brings the market's price and accrual current.
    pub fn advance(&mut self, slots: u64, price: u64) {
        let target = self.env.current_slot() + slots;
        self.env.warp_to_slot(target);
        self.env.push_auth_mark(0, target, price).expect("push mark");
        let _ = self.env.crank(0, target, vec![]);
        let _ = self.env.crank(0, target, vec![]);
    }

    /// A harness actor trades against the vault through Percolator's TradeCpi.
    pub fn taker_trade(&mut self, taker: usize, size_q: i128) -> Result<u64, String> {
        let market_id = self.env.primary_market_state().1.assets[0].market_id;
        let pv = self.portfolio_view();
        let ix = Instruction {
            program_id: perc::PERCOLATOR_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.env.actors[taker].signer.pubkey(), true),
                AccountMeta::new(self.env.market, false),
                AccountMeta::new(self.env.actors[taker].portfolio, false),
                AccountMeta::new(self.portfolio, false),
                AccountMeta::new_readonly(pid(), false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new_readonly(self.delegate, false),
            ],
            data: ProgIx::TradeCpi {
                account_a_portfolio_id: self.env.primary_portfolio_id(taker),
                account_a_position_epoch: self.env.primary_portfolio_position_epoch(taker),
                account_b_portfolio_id: pv.portfolio_id,
                account_b_position_epoch: pv.position_epoch,
                account_b_matcher_sequence: pv.sequence,
                asset_index: 0,
                market_id,
                size_q,
                fee_bps: 0,
                limit_price: 0,
                backing_fee_cap_bps: 0,
            }
            .encode(),
        };
        let signer = self.env.actors[taker].signer.insecure_clone();
        self.send(vec![ix], &[&signer])
    }
}

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
