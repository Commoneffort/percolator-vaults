//! Off-chain instruction builders for every router instruction (tests, keeper, tooling).

use percolator_vault::{client::VaultKeys, percolator as perc};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::{state::*, *};

fn w(k: Pubkey) -> AccountMeta {
    AccountMeta::new(k, false)
}
fn ro(k: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(k, false)
}

/// A trader's router addresses on a market.
#[derive(Clone, Copy, Debug)]
pub struct TraderKeys {
    pub wallet: Pubkey,
    pub market: Pubkey,
    pub trader: Pubkey,
    pub portfolio: Pubkey,
    pub collateral: Pubkey,
}

impl TraderKeys {
    pub fn new(market: Pubkey, wallet: Pubkey) -> Self {
        Self {
            wallet,
            market,
            trader: trader_address(&id(), &market, &wallet).0,
            portfolio: portfolio_address(&id(), &market, &wallet).0,
            collateral: collateral_address(&id(), &market, &wallet).0,
        }
    }
}

pub fn open_account(t: &TraderKeys, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new(t.wallet, true),
            w(t.trader),
            w(t.portfolio),
            w(t.collateral),
            w(t.market),
            ro(*mint),
            ro(perc::PERCOLATOR_PROGRAM_ID),
            ro(system_program::ID),
            ro(spl_token::ID),
        ],
        data: vec![TAG_OPEN_ACCOUNT],
    }
}

pub fn deposit(t: &TraderKeys, source: &Pubkey, percolator_vault: &Pubkey, amount: u64) -> Instruction {
    let mut data = vec![TAG_DEPOSIT];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new_readonly(t.wallet, true),
            ro(t.trader),
            w(*source),
            w(t.collateral),
            w(t.portfolio),
            w(t.market),
            w(*percolator_vault),
            ro(spl_token::ID),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data,
    }
}

pub fn withdraw(t: &TraderKeys, dest: &Pubkey, percolator_vault: &Pubkey, amount: u64) -> Instruction {
    let mut data = vec![TAG_WITHDRAW];
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new_readonly(t.wallet, true),
            ro(t.trader),
            w(t.portfolio),
            w(t.market),
            w(t.collateral),
            w(*dest),
            w(*percolator_vault),
            ro(perc::vault_authority(&t.market)),
            ro(spl_token::ID),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data,
    }
}

pub fn open_book(payer: &Pubkey, vault: &Pubkey) -> Instruction {
    Instruction {
        program_id: id(),
        accounts: vec![AccountMeta::new(*payer, true), ro(*vault), w(book_address(&id(), vault).0), ro(system_program::ID)],
        data: vec![TAG_OPEN_BOOK],
    }
}

/// `id` is the book's `next_id` when the request lands.
pub fn request(t: &TraderKeys, vault: &Pubkey, id: u64, size: i128) -> Instruction {
    let mut data = vec![TAG_REQUEST];
    data.extend_from_slice(&size.to_le_bytes());
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(t.wallet, true),
            w(t.trader),
            ro(*vault),
            w(book_address(&crate::id(), vault).0),
            w(request_address(&crate::id(), vault, id).0),
            ro(t.market),
            ro(t.portfolio),
            ro(system_program::ID),
        ],
        data,
    }
}

/// Queues the close-out of `t`'s position on a close-only market; `payer` need not be the trader.
pub fn request_close(payer: &Pubkey, t: &TraderKeys, vault: &Pubkey, id: u64) -> Instruction {
    Instruction {
        program_id: crate::id(),
        accounts: vec![
            AccountMeta::new(*payer, true),
            w(t.trader),
            ro(t.wallet),
            ro(*vault),
            w(book_address(&crate::id(), vault).0),
            w(request_address(&crate::id(), vault, id).0),
            ro(t.market),
            ro(system_program::ID),
            ro(t.portfolio),
        ],
        data: vec![TAG_REQUEST_CLOSE],
    }
}

pub fn advance(k: &VaultKeys, pyth: &Pubkey, observation_sequence: u64, authority_epoch: u64) -> Instruction {
    let mut data = vec![TAG_ADVANCE];
    data.extend_from_slice(&observation_sequence.to_le_bytes());
    data.extend_from_slice(&authority_epoch.to_le_bytes());
    Instruction {
        program_id: id(),
        accounts: vec![
            ro(authority_address(&id()).0),
            ro(k.vault),
            w(book_address(&id(), &k.vault).0),
            w(k.market),
            ro(*pyth),
            ro(percolator_vault::id()),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data,
    }
}

pub fn fill(executor: &Pubkey, k: &VaultKeys, t: &TraderKeys, request_id: u64) -> Instruction {
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new(*executor, true),
            w(request_address(&id(), &k.vault, request_id).0),
            w(t.trader),
            w(t.wallet),
            w(book_address(&id(), &k.vault).0),
            w(k.vault),
            w(k.market),
            w(t.portfolio),
            w(k.portfolio),
            ro(k.delegate),
            ro(authority_address(&id()).0),
            ro(percolator_vault::id()),
            ro(perc::PERCOLATOR_PROGRAM_ID),
        ],
        data: vec![TAG_FILL],
    }
}

pub fn expire(caller: &Pubkey, vault: &Pubkey, t: &TraderKeys, request_id: u64) -> Instruction {
    Instruction {
        program_id: id(),
        accounts: vec![
            AccountMeta::new_readonly(*caller, true),
            w(request_address(&id(), vault, request_id).0),
            w(t.trader),
            w(t.wallet),
            w(book_address(&id(), vault).0),
        ],
        data: vec![TAG_EXPIRE],
    }
}
