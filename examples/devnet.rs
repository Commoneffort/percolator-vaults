//! Devnet tooling: stands up a Percolator market with a vault that runs its own SOL/USD perp,
//! funds test users, runs the keeper, and prints status.
//!
//!   cargo run --example devnet -- setup-market          # mint, market, fee policies
//!   cargo run --example devnet -- create-vault          # vault in operate mode + SOL/USD listing
//!   cargo run --example devnet -- faucet <pubkey> <usdc>
//!   cargo run --example devnet -- keeper                # cranks, rolls, harvests, renews
//!   cargo run --example devnet -- status
//!
//! State goes to deploy/devnet.json. The payer/admin keypair is PERCOLATOR_KEYPAIR (default
//! ~/.config/solana/percolator-test/deployer.json); the RPC URL is DEVNET_RPC or the file
//! ~/.config/solana/percolator-test/devnet-rpc.

use percolator_prog::{ix::Instruction as ProgIx, state as pstate};
use percolator_vault::{
    client::{self, VaultKeys},
    percolator as perc,
    processor::{InitParams, ListArgs},
    state::{self, VaultState},
};
use serde_json::{json, Value};
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use std::{str::FromStr, thread::sleep, time::Duration};

const MANIFEST: &str = "deploy/devnet.json";
/// Pyth SOL/USD sponsored price account and feed id (same on devnet and mainnet).
const SOL_USD_ACCOUNT: &str = "7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE";
const SOL_USD_FEED: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const USDC: u64 = 1_000_000; // 6 decimals
const UNIT: u128 = 1_000_000; // Percolator POS_SCALE

fn rpc() -> RpcClient {
    let url = std::env::var("DEVNET_RPC").ok().unwrap_or_else(|| {
        let f = shellexpand("~/.config/solana/percolator-test/devnet-rpc");
        std::fs::read_to_string(f)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "https://api.devnet.solana.com".into())
    });
    RpcClient::new_with_commitment(url, CommitmentConfig::confirmed())
}

fn shellexpand(p: &str) -> String {
    p.replacen('~', &std::env::var("HOME").unwrap(), 1)
}

fn payer() -> Keypair {
    let p = std::env::var("PERCOLATOR_KEYPAIR")
        .unwrap_or_else(|_| shellexpand("~/.config/solana/percolator-test/deployer.json"));
    read_keypair_file(&p).expect("payer keypair")
}

fn send(rpc: &RpcClient, payer: &Keypair, ixs: Vec<Instruction>, extra: &[&Keypair]) -> Result<String, String> {
    let mut all = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(1_400_000),
        ComputeBudgetInstruction::set_compute_unit_price(20_000),
    ];
    all.extend(ixs);
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra);
    let bh = rpc.get_latest_blockhash().map_err(|e| e.to_string())?;
    let tx = Transaction::new_signed_with_payer(&all, Some(&payer.pubkey()), &signers, bh);
    rpc.send_and_confirm_transaction(&tx)
        .map(|s| s.to_string())
        .map_err(|e| format!("{e}"))
}

fn load() -> Value {
    std::fs::read_to_string(MANIFEST)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}))
}
fn save(v: &Value) {
    std::fs::create_dir_all("deploy").unwrap();
    std::fs::write(MANIFEST, serde_json::to_string_pretty(v).unwrap()).unwrap();
}
fn key(v: &Value, k: &str) -> Pubkey {
    Pubkey::from_str(v[k].as_str().unwrap_or_else(|| panic!("{k} missing from {MANIFEST}"))).unwrap()
}

fn market_state(rpc: &RpcClient, market: &Pubkey) -> (pstate::WrapperConfigV16, pstate::MarketGroupV16, Vec<u8>) {
    let data = rpc.get_account_data(market).expect("market account");
    let (c, g) = pstate::read_market(&data).expect("decode market");
    (c, g, data)
}

fn prog(ix: ProgIx, accounts: Vec<AccountMeta>) -> Instruction {
    Instruction { program_id: perc::PERCOLATOR_PROGRAM_ID, accounts, data: ix.encode() }
}

/// Current SOL/USD price in collateral atoms per unit (1 unit = 1 SOL, 1 atom = 1e-6 USD).
fn pyth_price_e6(rpc: &RpcClient) -> (u64, i64) {
    let d = rpc.get_account_data(&Pubkey::from_str(SOL_USD_ACCOUNT).unwrap()).unwrap();
    let mut off = 8 + 32;
    off += if d[off] == 1 { 1 } else { 2 };
    off += 32;
    let price = i64::from_le_bytes(d[off..off + 8].try_into().unwrap());
    let expo = i32::from_le_bytes(d[off + 16..off + 20].try_into().unwrap());
    let publish = i64::from_le_bytes(d[off + 20..off + 28].try_into().unwrap());
    let e6 = if expo <= -6 {
        price / 10i64.pow((-6 - expo) as u32)
    } else {
        price * 10i64.pow((expo + 6) as u32)
    };
    (e6 as u64, publish)
}

fn setup_market(rpc: &RpcClient, admin: &Keypair) {
    let mut m = load();
    // Collateral: a test USDC we can mint freely.
    let mint = Keypair::new();
    let rent = rpc.get_minimum_balance_for_rent_exemption(spl_token::state::Mint::LEN).unwrap();
    send(rpc, admin, vec![
        system_instruction::create_account(&admin.pubkey(), &mint.pubkey(), rent, spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint2(&spl_token::ID, &mint.pubkey(), &admin.pubkey(), None, 6).unwrap(),
    ], &[&mint]).expect("create tUSDC mint");
    println!("tUSDC mint {}", mint.pubkey());

    // Market account and Percolator's canonical collateral vault.
    let market = Keypair::new();
    let len = pstate::market_account_len_for_capacity(1).unwrap();
    let rent = rpc.get_minimum_balance_for_rent_exemption(len).unwrap();
    let vault_auth = perc::vault_authority(&market.pubkey());
    send(rpc, admin, vec![
        system_instruction::create_account(&admin.pubkey(), &market.pubkey(), rent, len as u64, &perc::PERCOLATOR_PROGRAM_ID),
        create_ata_idempotent(&admin.pubkey(), &vault_auth, &mint.pubkey()),
    ], &[&market]).expect("create market account");
    println!("market {}", market.pubkey());

    let init = ProgIx::InitMarket {
        max_portfolio_assets: 1,
        h_min: 0,
        h_max: 6_480_000,
        initial_price: USDC, // asset 0: the USD base unit, pinned at 1.0
        min_nonzero_mm_req: 500,
        min_nonzero_im_req: 600,
        maintenance_margin_bps: 1_000,
        initial_margin_bps: 1_000,
        max_trading_fee_bps: 10_000,
        trade_fee_base_bps: 0,
        liquidation_fee_bps: 5,
        liquidation_fee_cap: 50 * USDC as u128,
        min_liquidation_abs: 0,
        max_price_move_bps_per_slot: 49,
        max_accrual_dt_slots: 10,
        max_abs_funding_e9_per_slot: 0,
        min_funding_lifetime_slots: 10_000_000,
        max_account_b_settlement_chunks: 1,
        max_bankrupt_close_chunks: 1,
        max_bankrupt_close_lifetime_slots: 100,
        public_b_chunk_atoms: percolator::MAX_VAULT_TVL,
        maintenance_fee_per_slot: 0,
    };
    send(rpc, admin, vec![prog(init, vec![
        AccountMeta::new(admin.pubkey(), true),
        AccountMeta::new(market.pubkey(), false),
        AccountMeta::new_readonly(mint.pubkey(), false),
    ])], &[]).expect("InitMarket");

    let slot = rpc.get_slot().unwrap();
    let (_, g, data) = market_state(rpc, &market.pubkey());
    let seq = pstate::read_asset_control_sequences(&data, 0).unwrap();
    send(rpc, admin, vec![prog(ProgIx::ConfigureAuthMark {
        asset_index: 0,
        market_id: g.assets[0].market_id,
        now_slot: slot,
        initial_mark_e6: USDC,
        observation_sequence: seq.oracle_observation + 1,
        authority_epoch: seq.authority_epoch,
    }, vec![AccountMeta::new(admin.pubkey(), true), AccountMeta::new(market.pubkey(), false)])], &[])
        .expect("ConfigureAuthMark asset 0");

    // Permissionless listing costs 1 tUSDC (paid into asset-0 insurance); trades pay 5 bps.
    let (_, _, data) = market_state(rpc, &market.pubkey());
    let seq = pstate::read_asset_control_sequences(&data, 0).unwrap();
    send(rpc, admin, vec![
        prog(ProgIx::UpdateMarketInitFeePolicy { min_init_fee: USDC as u128, policy_sequence: seq.market_init_fee + 1, authority_epoch: seq.authority_epoch },
            vec![AccountMeta::new(admin.pubkey(), true), AccountMeta::new(market.pubkey(), false)]),
        prog(ProgIx::UpdateTradeFeePolicy { trade_fee_base_bps: 5, policy_sequence: seq.trade_fee + 1, authority_epoch: seq.authority_epoch },
            vec![AccountMeta::new(admin.pubkey(), true), AccountMeta::new(market.pubkey(), false)]),
    ], &[]).expect("fee policies");

    m["network"] = json!("devnet");
    m["percolator_program"] = json!(perc::PERCOLATOR_PROGRAM_ID.to_string());
    m["vault_program"] = json!(percolator_vault::id().to_string());
    m["collateral_mint"] = json!(mint.pubkey().to_string());
    m["market"] = json!(market.pubkey().to_string());
    m["percolator_vault"] = json!(client::associated_token_address(&vault_auth, &mint.pubkey()).to_string());
    m["percolator_vault_authority"] = json!(vault_auth.to_string());
    m["admin"] = json!(admin.pubkey().to_string());
    save(&m);
    println!("market ready; manifest in {MANIFEST}");
}

fn create_ata_idempotent(payer: &Pubkey, owner: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: client::ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(client::associated_token_address(owner, mint), false),
            AccountMeta::new_readonly(*owner, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        data: vec![1],
    }
}

fn keys(m: &Value, creator: &Pubkey) -> VaultKeys {
    let seed = m["vault_seed"].as_u64().unwrap_or(1);
    VaultKeys::derive(percolator_vault::id(), key(m, "market"), *creator, seed, &key(m, "collateral_mint"))
}

fn faucet(rpc: &RpcClient, admin: &Keypair, to: &Pubkey, amount: u64) -> Pubkey {
    let m = load();
    let mint = key(&m, "collateral_mint");
    let ata = client::associated_token_address(to, &mint);
    // The mint authority moved to a dedicated faucet key (keys/faucet.json) after setup.
    let authority = read_keypair_file("keys/faucet.json").unwrap_or_else(|_| admin.insecure_clone());
    send(rpc, admin, vec![
        create_ata_idempotent(&admin.pubkey(), to, &mint),
        spl_token::instruction::mint_to(&spl_token::ID, &mint, &ata, &authority.pubkey(), &[], amount).unwrap(),
    ], &[&authority]).expect("faucet");
    ata
}

fn create_vault(rpc: &RpcClient, admin: &Keypair, seed: u64) {
    let mut m = load();
    let market = key(&m, "market");
    let mint = key(&m, "collateral_mint");
    m["vault_seed"] = json!(seed);
    let k = keys(&m, &admin.pubkey());
    let (_, g, _) = market_state(rpc, &market);
    let mut feed = [0u8; 32];
    for i in 0..32 {
        feed[i] = u8::from_str_radix(&SOL_USD_FEED[2 * i..2 * i + 2], 16).unwrap();
    }
    let p = InitParams {
        seed,
        asset_index: 0,
        spread_bps: 10,
        unwind_spread_bps: 0,
        trade_fee_cap_bps: 10_000,
        backing_fee_cap_bps: 0,
        max_fill_abs: 100 * UNIT,       // 100 SOL per fill
        max_inventory_abs: 500 * UNIT,  // 500 SOL net
        epoch_len_slots: 1_500,         // about ten minutes
        matcher_ttl_slots: 216_000,     // about a day
        asset_generation_frontier: g.next_market_id,
        mode: state::MODE_OPERATE,
        insurance_floor: 100 * USDC,
        listing_fee_max: 2 * USDC,
        oracle_leg_count: 1,
        oracle_leg_flags: 0,
        oracle_invert: 0,
        oracle_unit_scale: 0,
        oracle_conf_filter_bps: 0,
        oracle_max_staleness_secs: 300,
        oracle_soft_stale_slots: 200,
        oracle_ewma_halflife_slots: 1,
        oracle_mark_min_fee: 0,
        oracle_feeds: [feed, [0; 32], [0; 32]],
    };
    send(rpc, admin, vec![client::init_vault(&k, &admin.pubkey(), &mint, &p)], &[]).expect("InitVault");
    println!("vault {}", k.vault);

    let payer_ata = faucet(rpc, admin, &admin.pubkey(), 10 * USDC);
    let (_, g, _) = market_state(rpc, &market);
    let (price, _) = pyth_price_e6(rpc);
    let args = ListArgs {
        asset_index: g.assets.len() as u16,
        market_id: g.next_market_id,
        activation_authority_epoch: 0,
        initial_price: price,
        oracle_observation_sequence: 1,
        oracle_authority_epoch: 0,
    };
    let sig = send(rpc, admin, vec![client::list_asset(&k, &admin.pubkey(), &payer_ata, &[Pubkey::from_str(SOL_USD_ACCOUNT).unwrap()], &args)], &[])
        .expect("ListAsset");
    println!("listed SOL/USD as asset {} ({sig})", args.asset_index);
    m["vault"] = json!(k.vault.to_string());
    m["vault_creator"] = json!(admin.pubkey().to_string());
    m["share_mint"] = json!(k.share_mint.to_string());
    m["buffer"] = json!(k.buffer.to_string());
    m["escrow"] = json!(k.escrow.to_string());
    m["lp_portfolio"] = json!(k.portfolio.to_string());
    m["matcher_delegate"] = json!(k.delegate.to_string());
    m["asset_index"] = json!(args.asset_index);
    m["oracle_account"] = json!(SOL_USD_ACCOUNT);
    save(&m);
}

fn vault_state(rpc: &RpcClient, vault: &Pubkey) -> VaultState {
    let d = rpc.get_account_data(vault).unwrap();
    bytemuck::pod_read_unaligned(&d[state::VAULT_STATE_OFF..])
}

/// Cranks the vault's asset until its accrual clock is current (each crank moves at most
/// `max_accrual_dt_slots`), reading the Pyth account, with the LP portfolio as the target.
fn crank(rpc: &RpcClient, payer: &Keypair, m: &Value, v: &VaultState) -> Result<(), String> {
    let market = key(m, "market");
    let oracle = Pubkey::from_str(SOL_USD_ACCOUNT).unwrap();
    for _ in 0..3 {
        let now = rpc.get_slot().map_err(|e| e.to_string())?;
        let (_, g, _) = market_state(rpc, &market);
        if g.assets[v.asset_index as usize].slot_last + 2 >= now {
            return Ok(());
        }
        let hint = percolator_prog::ix::CrankObservationHint { asset_index: v.asset_index, oracle_accounts: 1 };
        let ix = prog(ProgIx::PermissionlessCrank { now_slot: now, observations: vec![hint] }, vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(market, false),
            AccountMeta::new(v.lp_portfolio, false),
            AccountMeta::new_readonly(oracle, false),
        ]);
        if let Err(e) = send(rpc, payer, vec![ix], &[]) {
            // 0x16 = NonProgress: nothing left to do this slot.
            if e.contains("0x16") {
                return Ok(());
            }
            return Err(e);
        }
    }
    Ok(())
}

fn keeper(rpc: &RpcClient, payer: &Keypair) {
    let m = load();
    let creator = key(&m, "vault_creator");
    let k = keys(&m, &creator);
    let mut tick: u64 = 0;
    loop {
        tick += 1;
        let v = vault_state(rpc, &k.vault);
        let now = rpc.get_slot().unwrap_or(0);
        // Keep the vault's asset within Percolator's accrual window so takers can add risk.
        if let Err(e) = crank(rpc, payer, &m, &v) {
            eprintln!("crank: {}", e.lines().next().unwrap_or(""));
        }
        let epoch_over = now >= v.epoch_start_slot + v.epoch_len_slots;
        if tick % 15 != 0 && !epoch_over {
            sleep(Duration::from_secs(2));
            continue;
        }
        let (_, g, data) = market_state(rpc, &k.market);
        // Harvest fees above the floor.
        let (_, remaining) = {
            let base = perc::MARKET_SLOTS_OFF + v.asset_index as usize * perc::MARKET_ASSET_SLOT_LEN + 512;
            let rd = |o: usize| u128::from_le_bytes(data[base + o..base + o + 16].try_into().unwrap());
            (0, (rd(515) + rd(531)).saturating_sub(rd(547) + rd(563)))
        };
        if remaining > v.insurance_floor as u128 + USDC as u128 {
            let seq = pstate::read_asset_control_sequences(&data, v.asset_index as usize).unwrap();
            match send(rpc, payer, vec![client::harvest_fees(&k, seq.authority_epoch, u64::MAX)], &[]) {
                Ok(s) => println!("harvested fees ({s})"),
                Err(e) => eprintln!("harvest: {}", e.lines().next().unwrap_or("")),
            }
        }
        // Roll when the epoch is over (fails harmlessly until the vault is flat).
        if now >= v.epoch_start_slot + v.epoch_len_slots {
            let _ = send(rpc, payer, vec![client::convert_pnl(&k)], &[]);
            match send(rpc, payer, vec![client::roll_epoch(&k, &payer.pubkey(), v.epoch, g.next_market_id)], &[]) {
                Ok(s) => println!("rolled epoch {} ({s})", { v.epoch }),
                Err(e) => {
                    eprintln!("roll: {}", e.lines().next().unwrap_or(""));
                    if now >= v.epoch_start_slot + 2 * v.epoch_len_slots {
                        let _ = send(rpc, payer, vec![client::unwind(&k)], &[]);
                    }
                }
            }
        }
        sleep(Duration::from_secs(2));
    }
}

fn status(rpc: &RpcClient) {
    let m = load();
    let creator = key(&m, "vault_creator");
    let k = keys(&m, &creator);
    let v = vault_state(rpc, &k.vault);
    let (price, publish) = pyth_price_e6(rpc);
    let (_, g, _) = market_state(rpc, &k.market);
    let a = &g.assets[v.asset_index as usize];
    println!("vault {} status {} epoch {} inventory {} fills {} harvested {}", k.vault, { v.status }, { v.epoch }, { v.inventory }, { v.total_fills }, { v.total_fees_harvested });
    println!("asset {} effective {} target {} slot_last {} | pyth {} (publish {})", { v.asset_index }, a.effective_price, a.raw_oracle_target_price, a.slot_last, price, publish);
    println!("buffer {} reserved {} pending dep {} pending wd {}", rpc.get_token_account_balance(&k.buffer).map(|b| b.amount).unwrap_or_default(), { v.reserved_assets }, { v.pending_deposit_assets }, { v.pending_withdraw_shares });
}

/// The admin acting as a depositor (for seeding and demos).
fn user_op(rpc: &RpcClient, user: &Keypair, op: &str, amount: f64) {
    let m = load();
    let k = keys(&m, &key(&m, "vault_creator"));
    let mint = key(&m, "collateral_mint");
    let collateral = client::associated_token_address(&user.pubkey(), &mint);
    let shares = client::associated_token_address(&user.pubkey(), &k.share_mint);
    let ix = match op {
        "deposit" => {
            faucet(rpc, user, &user.pubkey(), (amount * USDC as f64) as u64);
            client::request_deposit(&k, &user.pubkey(), &collateral, (amount * USDC as f64) as u64)
        }
        "withdraw" => client::request_withdraw(&k, &user.pubkey(), &shares, amount as u64),
        _ => {
            send(rpc, user, vec![create_ata_idempotent(&user.pubkey(), &user.pubkey(), &k.share_mint)], &[]).unwrap();
            client::claim(&k, &user.pubkey(), amount as u64, &collateral, &shares)
        }
    };
    println!("{op}: {}", send(rpc, user, vec![ix], &[]).unwrap_or_else(|e| e));
}

/// The admin trades `units` SOL (positive = long) against the vault through Percolator.
fn trade(rpc: &RpcClient, taker: &Keypair, units: f64) {
    let mut m = load();
    let k = keys(&m, &key(&m, "vault_creator"));
    let mint = key(&m, "collateral_mint");
    let asset = m["asset_index"].as_u64().unwrap() as u16;
    let pf = if let Some(p) = m["taker_portfolio"].as_str() {
        Pubkey::from_str(p).unwrap()
    } else {
        // Create, initialize and fund a taker portfolio.
        let kp = Keypair::new();
        let len = perc::PORTFOLIO_ACCOUNT_LEN;
        let rent = rpc.get_minimum_balance_for_rent_exemption(len).unwrap();
        send(rpc, taker, vec![
            system_instruction::create_account(&taker.pubkey(), &kp.pubkey(), rent, len as u64, &perc::PERCOLATOR_PROGRAM_ID),
            perc::init_portfolio(&taker.pubkey(), &k.market, &kp.pubkey()),
        ], &[&kp]).expect("taker portfolio");
        let src = faucet(rpc, taker, &taker.pubkey(), 1_000 * USDC);
        let d = rpc.get_account_data(&kp.pubkey()).unwrap();
        let id = u64::from_le_bytes(d[perc::PORTFOLIO_ID_OFF..perc::PORTFOLIO_ID_OFF + 8].try_into().unwrap());
        let seq = u64::from_le_bytes(d[perc::PORTFOLIO_SEQUENCE_OFF..perc::PORTFOLIO_SEQUENCE_OFF + 8].try_into().unwrap());
        send(rpc, taker, vec![perc::deposit(&taker.pubkey(), &k.market, &kp.pubkey(), &src, &k.percolator_vault, id, seq, (1_000 * USDC) as u128)], &[])
            .expect("taker deposit");
        m["taker_portfolio"] = json!(kp.pubkey().to_string());
        save(&m);
        kp.pubkey()
    };
    let v = vault_state(rpc, &k.vault);
    crank(rpc, taker, &m, &v).ok();
    let read = |key: &Pubkey| {
        let d = rpc.get_account_data(key).unwrap();
        let control = u64::from_le_bytes(d[perc::PORTFOLIO_MATCHER_CONTROL_OFF..perc::PORTFOLIO_MATCHER_CONTROL_OFF + 8].try_into().unwrap());
        (
            u64::from_le_bytes(d[perc::PORTFOLIO_ID_OFF..perc::PORTFOLIO_ID_OFF + 8].try_into().unwrap()),
            (control >> 1) & ((1u64 << 49) - 1),
            u64::from_le_bytes(d[perc::PORTFOLIO_SEQUENCE_OFF..perc::PORTFOLIO_SEQUENCE_OFF + 8].try_into().unwrap()),
        )
    };
    let (a_id, a_epoch, _) = read(&pf);
    let (b_id, b_epoch, b_seq) = read(&k.portfolio);
    let (_, g, _) = market_state(rpc, &k.market);
    let ix = prog(ProgIx::TradeCpi {
        account_a_portfolio_id: a_id,
        account_a_position_epoch: a_epoch,
        account_b_portfolio_id: b_id,
        account_b_position_epoch: b_epoch,
        account_b_matcher_sequence: b_seq,
        asset_index: asset,
        market_id: g.assets[asset as usize].market_id,
        size_q: (units * UNIT as f64) as i128,
        fee_bps: 10_000,
        limit_price: 0,
        backing_fee_cap_bps: 0,
    }, vec![
        AccountMeta::new(taker.pubkey(), true),
        AccountMeta::new(k.market, false),
        AccountMeta::new(pf, false),
        AccountMeta::new(k.portfolio, false),
        AccountMeta::new_readonly(k.program, false),
        AccountMeta::new(k.vault, false),
        AccountMeta::new_readonly(k.delegate, false),
    ]);
    let _ = mint;
    println!("trade {units} SOL: {}", send(rpc, taker, vec![ix], &[]).unwrap_or_else(|e| e));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rpc = rpc();
    let admin = payer();
    match args.get(1).map(String::as_str) {
        Some("setup-market") => setup_market(&rpc, &admin),
        Some("create-vault") => create_vault(&rpc, &admin, args.get(2).map(|s| s.parse().unwrap()).unwrap_or(1)),
        Some("deposit") => user_op(&rpc, &admin, "deposit", args[2].parse::<f64>().unwrap()),
        Some("withdraw") => user_op(&rpc, &admin, "withdraw", args[2].parse::<f64>().unwrap()),
        Some("claim") => user_op(&rpc, &admin, "claim", args[2].parse::<f64>().unwrap()),
        Some("trade") => trade(&rpc, &admin, args[2].parse::<f64>().unwrap()),
        Some("faucet") => {
            let to = Pubkey::from_str(&args[2]).unwrap();
            let usdc: u64 = args[3].parse().unwrap();
            println!("{}", faucet(&rpc, &admin, &to, usdc * USDC));
        }
        Some("keeper") => keeper(&rpc, &admin),
        Some("status") => status(&rpc),
        _ => eprintln!("usage: devnet setup-market | create-vault | faucet <pubkey> <usdc> | keeper | status"),
    }
}
