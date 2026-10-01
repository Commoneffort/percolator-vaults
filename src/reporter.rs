//! Prices posted on chain by a reporter key, for when verified Pyth prices are not available.
//!
//! The router keeps one account per feed that only the reporter can write (`PostPrice`). Unlike
//! a Pyth update nothing on chain can check such a price: whoever holds the reporter key is
//! trusted for the price and for the publish times it reports. It is therefore used narrowly.
//! On Solana it is a fallback: the router accepts it only for a queued request that Pyth has
//! failed to serve for `FALLBACK_SECS`. In an X1 build (`x1` feature), where there is no Pyth, it
//! is the price source.

use solana_program::{account_info::AccountInfo, pubkey::Pubkey};

use crate::{error::VaultError, pyth::PriceUpdate, ROUTER_PROGRAM_ID};

/// The key allowed to post prices. Deployments use the operator's reporter key; the default
/// build (tests) uses a fixed test key (the ed25519 key of seed `[42; 32]`).
#[cfg(any(feature = "devnet", feature = "x1"))]
pub const REPORTER: Pubkey = solana_program::pubkey!("9KhhASTNPmDGy1M2TeLRhEEbFUzy8wgTz9g4cijViXuk");
#[cfg(not(any(feature = "devnet", feature = "x1")))]
pub const REPORTER: Pubkey = solana_program::pubkey!("2iXtA8oeZqUU5pofxK971TCEvFGfems2AcDRaZHKD2pQ");
/// How long a queued request's target must have passed without a Pyth price before a reported
/// price may serve it (Solana builds).
pub const FALLBACK_SECS: i64 = 20;

pub const REPORT_MAGIC: u64 = 0x5052_4943_4552_5054; // "PRICERPT"
pub const SEED_REPORT: &[u8] = b"report";
/// magic u64, feed [32], price_e6 u64, publish_time i64, prev_publish_time i64, bump u8, pad 7.
pub const REPORT_LEN: usize = 8 + 32 + 8 + 8 + 8 + 8;
pub const REPORT_FEED_OFF: usize = 8;
pub const REPORT_PRICE_OFF: usize = 40;
pub const REPORT_PUBLISH_OFF: usize = 48;
pub const REPORT_PREV_OFF: usize = 56;
pub const REPORT_BUMP_OFF: usize = 64;

/// The router's price account for a feed.
pub fn report_address(feed: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED_REPORT, feed], &ROUTER_PROGRAM_ID)
}

pub fn decode(data: &[u8]) -> Result<PriceUpdate, VaultError> {
    if data.len() != REPORT_LEN || data[..8] != REPORT_MAGIC.to_le_bytes() {
        return Err(VaultError::BadOracle);
    }
    let rd8 = |o: usize| <[u8; 8]>::try_from(&data[o..o + 8]).unwrap();
    let mut feed_id = [0u8; 32];
    feed_id.copy_from_slice(&data[REPORT_FEED_OFF..REPORT_FEED_OFF + 32]);
    let price = i64::try_from(u64::from_le_bytes(rd8(REPORT_PRICE_OFF))).map_err(|_| VaultError::BadOracle)?;
    Ok(PriceUpdate {
        feed_id,
        price,
        conf: 0,
        exponent: -6,
        publish_time: i64::from_le_bytes(rd8(REPORT_PUBLISH_OFF)),
        prev_publish_time: i64::from_le_bytes(rd8(REPORT_PREV_OFF)),
    })
}

/// Reads the reported price for `feed` from the router's account for it.
pub fn read(ai: &AccountInfo, feed: &[u8; 32]) -> Result<PriceUpdate, VaultError> {
    if *ai.owner != ROUTER_PROGRAM_ID || *ai.key != report_address(feed).0 {
        return Err(VaultError::BadOracle);
    }
    let u = decode(&ai.try_borrow_data().map_err(|_| VaultError::BadOracle)?)?;
    if u.feed_id != *feed {
        return Err(VaultError::BadOracle);
    }
    Ok(u)
}

/// True when `ai` is a reported-price account rather than a Pyth update.
pub fn is_report(ai: &AccountInfo) -> bool {
    *ai.owner == ROUTER_PROGRAM_ID
}
