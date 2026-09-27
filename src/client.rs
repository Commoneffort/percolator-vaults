//! Off-chain instruction builders for every vault instruction. Shared by the tests, the devnet
//! tooling and the keeper, so all of them encode exactly what the program decodes.

use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::{
    percolator as perc,
    processor::{self, InitParams, ListArgs},
    state,
};

/// Every address a vault uses, derived from its market, creator and seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaultKeys {
    pub program: Pubkey,
    pub market: Pubkey,
    pub creator: Pubkey,
    pub seed: u64,
    pub vault: Pubkey,
    pub share_mint: Pubkey,
    pub buffer: Pubkey,
    pub escrow: Pubkey,
    pub portfolio: Pubkey,
    pub delegate: Pubkey,
    /// Percolator's collateral vault for the market and its authority PDA.
    pub percolator_vault: Pubkey,
    pub percolator_vault_authority: Pubkey,
}

impl VaultKeys {
    pub fn derive(
        program: Pubkey,
        market: Pubkey,
        creator: Pubkey,
        seed: u64,
        collateral_mint: &Pubkey,
    ) -> Self {
        Self::with_vault(program, market, creator, seed, collateral_mint, state::vault_address(&program, &market, &creator, seed).0)
    }

    /// Keys of the canonical (operate-mode) vault for a feed.
    pub fn canonical(program: Pubkey, market: Pubkey, feed: &[u8; 32], collateral_mint: &Pubkey) -> Self {
        let vault = state::canonical_vault_address(&program, &market, feed).0;
        Self::with_vault(program, market, Pubkey::default(), 0, collateral_mint, vault)
    }

    /// Keys for a vault whose own address is already known (read it from the account).
    pub fn with_vault(
        program: Pubkey,
        market: Pubkey,
        creator: Pubkey,
        seed: u64,
        collateral_mint: &Pubkey,
        vault: Pubkey,
    ) -> Self {
        let child = |t| state::child_address(&program, t, &vault).0;
        let portfolio = child(state::SEED_PORTFOLIO);
        let percolator_vault_authority = perc::vault_authority(&market);
        Self {
            program,
            market,
            creator,
            seed,
            vault,
            share_mint: child(state::SEED_SHARES),
            buffer: child(state::SEED_BUFFER),
            escrow: child(state::SEED_ESCROW),
            portfolio,
            delegate: perc::matcher_delegate(&market, &portfolio, &vault, &program, &vault),
            percolator_vault: associated_token_address(&percolator_vault_authority, collateral_mint),
            percolator_vault_authority,
        }
    }

    pub fn ticket(&self, user: &Pubkey) -> Pubkey {
        state::ticket_address(&self.program, &self.vault, user).0
    }

    pub fn epoch_record(&self, epoch: u64) -> Pubkey {
        state::epoch_address(&self.program, &self.vault, epoch).0
    }
}

pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

pub fn associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), spl_token::ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

fn ro(k: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(k, false)
}
fn w(k: Pubkey) -> AccountMeta {
    AccountMeta::new(k, false)
}

pub fn init_vault(k: &VaultKeys, payer: &Pubkey, collateral_mint: &Pubkey, p: &InitParams) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(k.creator, true),
            w(k.vault),
            w(k.market),
            ro(*collateral_mint),
            w(k.share_mint),
            w(k.buffer),
            w(k.escrow),
            w(k.portfolio),
            ro(k.delegate),
            ro(perc::PERCOLATOR_PROGRAM_ID),
            ro(k.program),
            ro(spl_token::ID),
            ro(system_program::ID),
        ],
        data: p.encode(),
    }
}

pub fn list_asset(k: &VaultKeys, payer: &Pubkey, payer_collateral: &Pubkey, oracles: &[Pubkey], a: &ListArgs) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        w(*payer_collateral),
        w(k.vault),
        w(k.market),
        w(k.buffer),
        w(k.percolator_vault),
        ro(spl_token::ID),
        ro(system_program::ID),
        ro(perc::PERCOLATOR_PROGRAM_ID),
    ];
    accounts.extend(oracles.iter().map(|o| ro(*o)));
    Instruction { program_id: k.program, accounts, data: a.encode() }
}

fn with_u64(tag: u8, v: u64) -> Vec<u8> {
    let mut d = vec![tag];
    d.extend_from_slice(&v.to_le_bytes());
    d
}

pub fn request_deposit(k: &VaultKeys, user: &Pubkey, user_collateral: &Pubkey, amount: u64) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new(*user, true),
            w(k.vault),
            w(k.ticket(user)),
            w(*user_collateral),
            w(k.buffer),
            ro(spl_token::ID),
            ro(system_program::ID),
        ],
        data: with_u64(processor::TAG_REQUEST_DEPOSIT, amount),
    }
}

pub fn request_withdraw(k: &VaultKeys, user: &Pubkey, user_shares: &Pubkey, shares: u64) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new(*user, true),
            w(k.vault),
            w(k.ticket(user)),
            w(*user_shares),
            w(k.escrow),
            ro(spl_token::ID),
            ro(system_program::ID),
        ],
        data: with_u64(processor::TAG_REQUEST_WITHDRAW, shares),
    }
}

pub fn roll_epoch(k: &VaultKeys, cranker: &Pubkey, epoch: u64, frontier: u64) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new(*cranker, true),
            w(k.vault),
            w(k.market),
            w(k.portfolio),
            w(k.buffer),
            w(k.percolator_vault),
            ro(k.percolator_vault_authority),
            w(k.share_mint),
            w(k.escrow),
            w(k.epoch_record(epoch)),
            ro(k.delegate),
            ro(perc::PERCOLATOR_PROGRAM_ID),
            ro(k.program),
            ro(spl_token::ID),
            ro(system_program::ID),
        ],
        data: with_u64(processor::TAG_ROLL_EPOCH, frontier),
    }
}

pub fn claim(k: &VaultKeys, user: &Pubkey, epoch: u64, user_collateral: &Pubkey, user_shares: &Pubkey) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new_readonly(*user, true),
            w(k.vault),
            w(k.ticket(user)),
            ro(k.epoch_record(epoch)),
            w(*user_collateral),
            w(*user_shares),
            w(k.buffer),
            w(k.escrow),
            ro(spl_token::ID),
        ],
        data: vec![processor::TAG_CLAIM],
    }
}

pub fn refresh_matcher(k: &VaultKeys, frontier: u64) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            ro(k.vault),
            ro(k.market),
            w(k.portfolio),
            ro(k.program),
            ro(k.delegate),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data: with_u64(processor::TAG_REFRESH_MATCHER, frontier),
    }
}

pub fn convert_pnl(k: &VaultKeys) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![ro(k.vault), w(k.market), w(k.portfolio), ro(perc::PERCOLATOR_PROGRAM_ID)],
        data: vec![processor::TAG_CONVERT_PNL],
    }
}

pub fn harvest_fees(k: &VaultKeys, authority_epoch: u64, max_amount: u64) -> Instruction {
    let mut data = with_u64(processor::TAG_HARVEST_FEES, authority_epoch);
    data.extend_from_slice(&max_amount.to_le_bytes());
    Instruction {
        program_id: k.program,
        accounts: vec![
            w(k.vault),
            w(k.market),
            w(k.buffer),
            w(k.percolator_vault),
            ro(k.percolator_vault_authority),
            ro(spl_token::ID),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data,
    }
}

pub fn unwind(k: &VaultKeys) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![w(k.vault), w(k.market), w(k.portfolio), ro(perc::PERCOLATOR_PROGRAM_ID)],
        data: vec![processor::TAG_UNWIND],
    }
}

pub fn settle_resolved(k: &VaultKeys) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            w(k.vault),
            w(k.market),
            w(k.portfolio),
            w(k.buffer),
            w(k.percolator_vault),
            ro(k.percolator_vault_authority),
            ro(spl_token::ID),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data: vec![processor::TAG_SETTLE_RESOLVED],
    }
}

pub fn redeem_terminal(k: &VaultKeys, user: &Pubkey, user_shares: &Pubkey, user_collateral: &Pubkey, shares: u64) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![
            AccountMeta::new_readonly(*user, true),
            w(k.vault),
            w(*user_shares),
            w(k.share_mint),
            w(*user_collateral),
            w(k.buffer),
            ro(spl_token::ID),
        ],
        data: with_u64(processor::TAG_REDEEM_TERMINAL, shares),
    }
}

pub fn sweep(k: &VaultKeys, stray: &Pubkey) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![ro(k.vault), w(*stray), w(k.buffer), ro(spl_token::ID)],
        data: vec![processor::TAG_SWEEP],
    }
}

pub fn claim_opener_fees(k: &VaultKeys, opener: &Pubkey, dest: &Pubkey) -> Instruction {
    Instruction {
        program_id: k.program,
        accounts: vec![AccountMeta::new_readonly(*opener, true), w(k.vault), w(*dest), w(k.buffer), ro(spl_token::ID)],
        data: vec![processor::TAG_CLAIM_OPENER_FEES],
    }
}

/// Hands the market's `marketauth` to the program's governor PDA (signed by the current one).
pub fn accept_governance(program: &Pubkey, current: &Pubkey, market: &Pubkey, authority_epoch: u64) -> Instruction {
    Instruction {
        program_id: *program,
        accounts: vec![
            AccountMeta::new_readonly(*current, true),
            ro(state::governor_address(program, market).0),
            w(*market),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data: with_u64(processor::TAG_ACCEPT_GOVERNANCE, authority_epoch),
    }
}

/// Retires an idle vault's market (no depositors, no requests, no position, listed long enough).
pub fn retire_market(k: &VaultKeys, asset_authority_epoch: u64, market_authority_epoch: u64) -> Instruction {
    let mut data = with_u64(processor::TAG_RETIRE_MARKET, asset_authority_epoch);
    data.extend_from_slice(&market_authority_epoch.to_le_bytes());
    Instruction {
        program_id: k.program,
        accounts: vec![
            w(k.vault),
            w(k.market),
            ro(state::governor_address(&k.program, &k.market).0),
            ro(k.share_mint),
            ro(k.portfolio),
            w(k.buffer),
            w(k.percolator_vault),
            ro(k.percolator_vault_authority),
            ro(spl_token::ID),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data,
    }
}
