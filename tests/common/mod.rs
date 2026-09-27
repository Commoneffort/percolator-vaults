//! Shared test world: the vault program and the production Percolator SBF binary together in
//! LiteSVM, using Percolator's own test harness to build the market.

#![allow(dead_code)]

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../../../percolator-prog/tests/support/v16_svm.rs"]
pub mod v16_svm;

use litesvm::LiteSVM;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_router::client::{self as rclient, TraderKeys};
use percolator_vault::{
    client,
    percolator as perc,
    processor::{self, InitParams},
    state::{self, VaultState},
};
use std::collections::HashMap;
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
pub use v16_svm::{MarketConfig, V16Svm, INITIAL_PRICE};

const VAULT_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/percolator_vault.so");
const ROUTER_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/percolator_router.so");
pub const EPOCH_LEN: u64 = 20;
pub const SPREAD_BPS: u16 = 50;
/// One unit of position size (Percolator's POS_SCALE).
pub const UNIT: u128 = 1_000_000;
/// 10 units: 10M collateral atoms of notional at the initial price (1M atoms per unit).
pub const TEN_M: i128 = 10 * UNIT as i128;

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
    pub keys: Option<client::VaultKeys>,
    /// Publish time and price (e6) of the last Pyth update the harness published.
    pub pyth_time: i64,
    pub price: u64,
    /// Router trading accounts of the harness takers, by taker index.
    pub traders: HashMap<usize, (User, TraderKeys)>,
}

impl World {
    /// Lists the vault's own asset, paid by `lister`, with `pyth` as its price account.
    pub fn list_asset(&mut self, lister: &User, pyth: Pubkey) -> Result<u64, String> {
        let asset_index = self.env.primary_market_state().1.assets.len() as u16;
        self.list_asset_at(lister, pyth, asset_index)
    }

    /// Lists the vault's own asset in slot `asset_index` (a new slot, or a retired one to reuse).
    pub fn list_asset_at(&mut self, lister: &User, pyth: Pubkey, asset_index: u16) -> Result<u64, String> {
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
        let cu = self.send(vec![ix], &[&lister.kp])?;
        // Keep the harness's Pyth clock in step with the update the vault listed at, and open
        // the vault's router book (once).
        let u = percolator_vault::pyth::decode(&self.env.svm.get_account(&pyth).unwrap().data).unwrap();
        self.pyth_time = self.pyth_time.max(u.publish_time);
        self.price = u.price_e6().unwrap();
        let book = percolator_router::state::book_address(&percolator_router::id(), &self.vault).0;
        if self.env.svm.get_account(&book).map_or(true, |a| a.data.is_empty()) {
            let ix = rclient::open_book(&self.payer.pubkey(), &self.vault);
            self.send(vec![ix], &[])?;
        }
        Ok(cu)
    }

    /// Creates an operate-mode vault for `FEED`, enables listing and lists it at the initial price.
    pub fn operate(&mut self) -> User {
        self.env.update_market_init_fee_policy(LISTING_FEE as u128).unwrap();
        self.create_vault(operate_params(2)).unwrap();
        let lister = self.new_user(10_000_000);
        let ts = self.clock_ts().max(self.pyth_time);
        self.pyth_time = ts;
        let pyth = self.pyth_update(INITIAL_PRICE, ts, ts - 1);
        self.list_asset(&lister, pyth).unwrap();
        lister
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
    /// Publishes a fully verified Pyth update for `FEED` (a new receiver-owned account).
    pub fn pyth_update(&mut self, price_e6: u64, publish_time: i64, prev_publish_time: i64) -> Pubkey {
        let mut d = vec![0u8; 134];
        d[..8].copy_from_slice(&[0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd]);
        d[40] = 1;
        d[41..73].copy_from_slice(&FEED);
        d[73..81].copy_from_slice(&(price_e6 as i64).to_le_bytes());
        d[89..93].copy_from_slice(&(-6i32).to_le_bytes());
        d[93..101].copy_from_slice(&publish_time.to_le_bytes());
        d[101..109].copy_from_slice(&prev_publish_time.to_le_bytes());
        let key = Pubkey::new_unique();
        self.env
            .svm
            .set_account(key, Account { lamports: 1_000_000_000, data: d, owner: percolator_vault::pyth::PYTH_RECEIVER_PROGRAM_ID, executable: false, rent_epoch: 0 })
            .unwrap();
        key
    }

    /// The next Pyth update after the last one: `secs` later, at `price_e6`.
    pub fn next_pyth(&mut self, secs: i64, price_e6: u64) -> Pubkey {
        let prev = self.pyth_time;
        self.pyth_time += secs;
        self.price = price_e6;
        self.pyth_update(price_e6, self.pyth_time, prev)
    }

    /// Moves the clock to `slots` later and the Pyth clock with it (unix time never runs behind Pyth).
    pub fn tick(&mut self, slots: u64) {
        let slot = self.env.current_slot() + slots;
        let ts = self.clock_ts().max(self.pyth_time) + ((slots as i64 * 2) / 5).max(1);
        self.env.set_clock(slot, ts);
    }

    pub fn clock_ts(&self) -> i64 {
        self.env.svm.get_sysvar::<solana_sdk::clock::Clock>().unix_timestamp
    }

    pub fn asset(&self) -> u16 {
        self.vault_state().asset_index
    }

    pub fn book(&self) -> percolator_router::state::Book {
        let k = percolator_router::state::book_address(&percolator_router::id(), &self.vault).0;
        bytemuck::pod_read_unaligned(&self.env.svm.get_account(&k).unwrap().data)
    }

    /// Router `Advance` to a Pyth update.
    pub fn advance_ix(&self, pyth: &Pubkey) -> Instruction {
        let seq = self.env.primary_control_sequences(self.asset() as usize);
        rclient::advance(self.keys.as_ref().unwrap(), pyth, seq.oracle_observation + 1, seq.authority_epoch)
    }

    pub fn advance_to(&mut self, pyth: Pubkey) -> Result<u64, String> {
        let ix = self.advance_ix(&pyth);
        self.send(vec![ix], &[])
    }

    /// Moves time forward, publishes a new Pyth price, moves the mark to it through the router and
    /// cranks until Percolator's effective price has reached it.
    pub fn advance_oracle(&mut self, _pyth: Pubkey, slots: u64, _ts: &mut i64, price: i64) {
        self.move_price(slots, (price as u64) / 100);
    }

    /// Moves time forward by `slots`, then the mark to `price_e6` (a fresh Pyth update), converged.
    pub fn move_price(&mut self, slots: u64, price_e6: u64) {
        self.tick(slots);
        let secs = (self.clock_ts() - self.pyth_time).max(1);
        let u = self.next_pyth(secs, price_e6);
        self.advance_to(u).expect("advance mark");
        self.converge();
    }

    /// Cranks the vault's asset slot by slot until Percolator's effective price equals the mark
    /// (it moves at most its per-slot cap toward it).
    pub fn converge(&mut self) {
        let asset = self.asset();
        for _ in 0..200 {
            self.tick(1);
            let slot = self.env.current_slot();
            self.catch_up(asset, slot, &[]);
            let eff = self.env.primary_market_state().1.assets[asset as usize].effective_price;
            if eff == self.book().mark_price {
                self.settle();
                return;
            }
        }
        panic!("effective price did not converge to the mark");
    }

    /// Settles every position a price move left out of date (as keepers and executors do):
    /// a permissionless crank on each router trader portfolio and the vault's own, at this slot.
    pub fn settle(&mut self) {
        let asset = self.asset();
        let slot = self.env.current_slot();
        let mut portfolios: Vec<Pubkey> = self.traders.values().map(|(_, t)| t.portfolio).collect();
        portfolios.push(self.portfolio);
        for pf in portfolios {
            let ix = Instruction {
                program_id: perc::PERCOLATOR_PROGRAM_ID,
                accounts: vec![
                    AccountMeta::new(self.payer.pubkey(), true),
                    AccountMeta::new(self.env.market, false),
                    AccountMeta::new(pf, false),
                ],
                data: ProgIx::PermissionlessCrank {
                    now_slot: slot,
                    observations: vec![percolator_prog::ix::CrankObservationHint { asset_index: asset, oracle_accounts: 0 }],
                }
                .encode(),
            };
            let _ = self.send(vec![ix], &[]); // NonProgress when the leg is already current
        }
    }

    /// A router trading account for harness taker `taker`, funded once.
    pub fn trader(&mut self, taker: usize) -> TraderKeys {
        if let Some((_, t)) = self.traders.get(&taker) {
            return *t;
        }
        let u = self.new_user(10_000_000_000);
        let t = TraderKeys::new(self.env.market, u.kp.pubkey());
        let mint = self.env.mint;
        let open = rclient::open_account(&t, &mint);
        let dep = rclient::deposit(&t, &u.collateral, &self.env.vault, 1_000_000_000);
        let kp = u.kp.insecure_clone();
        self.send(vec![open, dep], &[&kp]).expect("open and fund router account");
        self.traders.insert(taker, (u, t));
        t
    }

    pub fn trader_kp(&self, taker: usize) -> Keypair {
        self.traders[&taker].0.kp.insecure_clone()
    }

    /// Queues a trade for `taker`; returns the request id.
    pub fn request(&mut self, taker: usize, size_q: i128) -> Result<u64, String> {
        let t = self.trader(taker);
        let id = self.book().next_id;
        let ix = rclient::request(&t, &self.vault, id, size_q);
        let kp = self.trader_kp(taker);
        self.send(vec![ix], &[&kp])?;
        Ok(id)
    }

    pub fn request_target(&self, id: u64) -> i64 {
        let k = percolator_router::state::request_address(&percolator_router::id(), &self.vault, id).0;
        let r: percolator_router::state::Request = bytemuck::pod_read_unaligned(&self.env.svm.get_account(&k).unwrap().data);
        r.target_time
    }

    pub fn fill_ix(&self, taker: usize, id: u64, executor: &Pubkey) -> Instruction {
        rclient::fill(executor, self.keys.as_ref().unwrap(), &self.traders[&taker].1, id)
    }

    /// Fills a request whose target the mark is at (executed by the payer).
    pub fn fill(&mut self, taker: usize, id: u64) -> Result<u64, String> {
        let ix = self.fill_ix(taker, id, &self.payer.pubkey());
        self.send(vec![ix], &[])
    }

    /// The full router path, as keepers run it: request, then the first Pyth update at the target
    /// (at the current price), advance, converge, fill. Returns the fill's compute units.
    pub fn taker_trade_asset(&mut self, taker: usize, _asset: u16, size_q: i128) -> Result<u64, String> {
        let id = self.request(taker, size_q)?;
        let target = self.request_target(id);
        let slot = self.env.current_slot() + 1;
        self.env.set_clock(slot, target.max(self.clock_ts()));
        let prev = self.pyth_time;
        self.pyth_time = target;
        let u = self.pyth_update(self.price, target, prev);
        self.advance_to(u)?;
        self.converge();
        self.fill(taker, id)
    }

    /// A taker's router portfolio (for position checks).
    pub fn taker_portfolio(&self, taker: usize) -> Pubkey {
        self.traders[&taker].1.portfolio
    }
}

pub struct User {
    pub kp: Keypair,
    pub collateral: Pubkey,
    pub shares: Pubkey,
}

pub fn pid() -> Pubkey {
    percolator_vault::id()
}

pub fn set_token(svm: &mut LiteSVM, key: Pubkey, mint: Pubkey, owner: Pubkey, amount: u64) {
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
        max_fill_abs: 40 * UNIT,
        max_inventory_abs: 45 * UNIT,
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
        position_nav_bps: 0,
        fill_nav_bps: 0,
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
        let router = std::fs::read(ROUTER_SO).expect("build the router with cargo build-sbf first");
        env.svm.add_program(percolator_router::id(), &router);
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
            keys: None,
            pyth_time: 1_000,
            price: INITIAL_PRICE,
            traders: HashMap::new(),
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
        let (vault, _) = if p.mode == state::MODE_OPERATE {
            state::canonical_vault_address(&pid(), &market, &p.oracle_feeds[0])
        } else {
            state::vault_address(&pid(), &market, &self.creator.pubkey(), p.seed)
        };
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
        self.vault = if p.mode == state::MODE_OPERATE {
            state::canonical_vault_address(&pid(), &market, &p.oracle_feeds[0]).0
        } else {
            state::vault_address(&pid(), &market, &creator.pubkey(), p.seed).0
        };
        let child = |t| state::child_address(&pid(), t, &self.vault).0;
        self.share_mint = child(state::SEED_SHARES);
        self.buffer = child(state::SEED_BUFFER);
        self.escrow = child(state::SEED_ESCROW);
        self.portfolio = child(state::SEED_PORTFOLIO);
        self.delegate = perc::matcher_delegate(&market, &self.portfolio, &self.vault, &pid(), &self.vault);
        let keys = if p.mode == state::MODE_OPERATE {
            client::VaultKeys::canonical(pid(), market, &p.oracle_feeds[0], &self.env.mint)
        } else {
            client::VaultKeys::derive(pid(), market, creator.pubkey(), p.seed, &self.env.mint)
        };
        assert_eq!(keys.vault, self.vault);
        assert_eq!(keys.percolator_vault, self.env.vault, "canonical Percolator vault ATA");
        self.keys = Some(keys);
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
        client::request_deposit(self.keys.as_ref().unwrap(), &u.kp.pubkey(), &u.collateral, amount)
    }

    pub fn deposit(&mut self, u: &User, amount: u64) -> Result<u64, String> {
        let ix = self.deposit_ix(u, amount);
        self.send(vec![ix], &[&u.kp])
    }

    pub fn withdraw(&mut self, u: &User, shares: u64) -> Result<u64, String> {
        let ix = client::request_withdraw(self.keys.as_ref().unwrap(), &u.kp.pubkey(), &u.shares, shares);
        self.send(vec![ix], &[&u.kp])
    }

    pub fn roll_ix(&self, cranker: &Pubkey) -> Instruction {
        let epoch = self.vault_state().epoch;
        client::roll_epoch(self.keys.as_ref().unwrap(), cranker, epoch, self.frontier())
    }

    pub fn roll(&mut self) -> Result<u64, String> {
        let ix = self.roll_ix(&self.payer.pubkey());
        self.send(vec![ix], &[])
    }

    pub fn claim_ix(&self, u: &User, epoch: u64) -> Instruction {
        client::claim(self.keys.as_ref().unwrap(), &u.kp.pubkey(), epoch, &u.collateral, &u.shares)
    }

    pub fn claim(&mut self, u: &User, epoch: u64) -> Result<u64, String> {
        let ix = self.claim_ix(u, epoch);
        self.send(vec![ix], &[&u.kp])
    }

    /// Moves time forward and the vault asset's mark to `price` (through the router), converged.
    pub fn advance(&mut self, slots: u64, price: u64) {
        self.move_price(slots, price);
    }

    /// Cranks an asset until its accrual clock reaches `slot`, as a keeper would: each crank
    /// advances accrual by at most `max_accrual_dt_slots`.
    pub fn catch_up(&mut self, asset: u16, slot: u64, oracles: &[Pubkey]) {
        for _ in 0..64 {
            let hint = percolator_prog::ix::CrankObservationHint {
                asset_index: asset,
                oracle_accounts: oracles.len() as u8,
            };
            let r = self.env.crank_with_oracles(0, slot, vec![hint], oracles);
            let last = self.env.primary_market_state().1.assets[asset as usize].slot_last;
            if last >= slot {
                return;
            }
            r.expect("keeper crank");
        }
        panic!("asset {asset} did not catch up to slot {slot}");
    }

    /// A harness taker trades against the vault through the router.
    pub fn taker_trade(&mut self, taker: usize, size_q: i128) -> Result<u64, String> {
        let asset = self.asset();
        self.taker_trade_asset(taker, asset, size_q)
    }
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
