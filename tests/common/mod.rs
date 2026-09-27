//! Shared test world: the vault program and the production Percolator SBF binary together in
//! LiteSVM, using Percolator's own test harness to build the market.

#![allow(dead_code)]

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../../../percolator-prog/tests/support/v16_svm.rs"]
pub mod v16_svm;

use litesvm::LiteSVM;
use percolator_prog::ix::Instruction as ProgIx;
use percolator_vault::{
    client,
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
pub use v16_svm::{MarketConfig, V16Svm, INITIAL_PRICE};

const VAULT_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/percolator_vault.so");
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
        self.catch_up(asset, slot, &[pyth]);
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

    /// Moves time forward and brings the market's price and accrual current.
    pub fn advance(&mut self, slots: u64, price: u64) {
        let target = self.env.current_slot() + slots;
        self.env.warp_to_slot(target);
        self.env.push_auth_mark(0, target, price).expect("push mark");
        self.catch_up(0, target, &[]);
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

