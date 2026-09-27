//! Reads Pyth `PriceUpdateV2` accounts (the Solana receiver's format, pull or sponsored push).
//!
//! Only accounts owned by the Pyth receiver program are accepted, and only fully verified
//! updates (all Wormhole guardian signatures checked by the receiver), so the price, its publish
//! time and the previous publish time are exactly what Pyth signed.

use solana_program::{account_info::AccountInfo, pubkey::Pubkey};

use crate::error::VaultError;

pub const PYTH_RECEIVER_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
const DISCRIMINATOR: [u8; 8] = [0x22, 0xf1, 0x23, 0x63, 0x9d, 0x7e, 0xf4, 0xcd];
const VERIFICATION_OFF: usize = 40;
const VERIFIED_FULL: u8 = 1;
const MESSAGE_OFF: usize = 41; // a fully verified update has a one-byte verification level
const MIN_LEN: usize = MESSAGE_OFF + 32 + 8 + 8 + 4 + 8 + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PriceUpdate {
    pub feed_id: [u8; 32],
    pub price: i64,
    pub conf: u64,
    pub exponent: i32,
    /// Pyth publish time of this price (unix seconds).
    pub publish_time: i64,
    /// Publish time of the price Pyth published immediately before this one. An update with
    /// `prev_publish_time < t <= publish_time` is the unique first price published at or after `t`.
    pub prev_publish_time: i64,
}

impl PriceUpdate {
    /// Price in e6 quote units per whole unit (Percolator's price scale), or `None` if it is not
    /// positive or does not fit.
    pub fn price_e6(&self) -> Option<u64> {
        if self.price <= 0 {
            return None;
        }
        let p = self.price as u128;
        let e = self.exponent + 6;
        let v = if e >= 0 {
            p.checked_mul(10u128.checked_pow(e as u32)?)?
        } else {
            p / 10u128.checked_pow((-e) as u32)?
        };
        u64::try_from(v).ok().filter(|v| *v != 0)
    }

    /// True when this is the first price Pyth published at or after `t`.
    pub fn is_first_at_or_after(&self, t: i64) -> bool {
        self.prev_publish_time < t && t <= self.publish_time
    }
}

pub fn decode(data: &[u8]) -> Result<PriceUpdate, VaultError> {
    if data.len() < MIN_LEN || data[..8] != DISCRIMINATOR || data[VERIFICATION_OFF] != VERIFIED_FULL {
        return Err(VaultError::BadOracle);
    }
    let rd8 = |o: usize| <[u8; 8]>::try_from(&data[o..o + 8]).unwrap();
    let m = MESSAGE_OFF;
    let mut feed_id = [0u8; 32];
    feed_id.copy_from_slice(&data[m..m + 32]);
    Ok(PriceUpdate {
        feed_id,
        price: i64::from_le_bytes(rd8(m + 32)),
        conf: u64::from_le_bytes(rd8(m + 40)),
        exponent: i32::from_le_bytes(data[m + 48..m + 52].try_into().unwrap()),
        publish_time: i64::from_le_bytes(rd8(m + 52)),
        prev_publish_time: i64::from_le_bytes(rd8(m + 60)),
    })
}

/// Reads a verified update for `feed` from an account owned by the Pyth receiver.
pub fn read(ai: &AccountInfo, feed: &[u8; 32]) -> Result<PriceUpdate, VaultError> {
    if *ai.owner != PYTH_RECEIVER_PROGRAM_ID {
        return Err(VaultError::BadOracle);
    }
    let u = decode(&ai.try_borrow_data().map_err(|_| VaultError::BadOracle)?)?;
    if u.feed_id != *feed {
        return Err(VaultError::BadOracle);
    }
    Ok(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(price: i64, expo: i32, publish: i64, prev: i64) -> Vec<u8> {
        let mut d = vec![0u8; 134];
        d[..8].copy_from_slice(&DISCRIMINATOR);
        d[40] = 1;
        d[41..73].copy_from_slice(&[7; 32]);
        d[73..81].copy_from_slice(&price.to_le_bytes());
        d[89..93].copy_from_slice(&expo.to_le_bytes());
        d[93..101].copy_from_slice(&publish.to_le_bytes());
        d[101..109].copy_from_slice(&prev.to_le_bytes());
        d
    }

    #[test]
    fn decodes_and_scales() {
        let u = decode(&update(12_152_825_290, -8, 100, 99)).unwrap();
        assert_eq!(u.price_e6(), Some(121_528_252));
        assert_eq!(decode(&update(5, 2, 1, 0)).unwrap().price_e6(), Some(500_000_000));
        assert_eq!(decode(&update(-1, -8, 1, 0)).unwrap().price_e6(), None);
    }

    #[test]
    fn first_at_or_after_is_unique() {
        let u = decode(&update(1, 0, 105, 102)).unwrap();
        assert!(!u.is_first_at_or_after(102));
        assert!(u.is_first_at_or_after(103));
        assert!(u.is_first_at_or_after(105));
        assert!(!u.is_first_at_or_after(106));
    }

    #[test]
    fn rejects_partial_verification() {
        let mut d = update(1, 0, 1, 0);
        d[40] = 0;
        assert!(decode(&d).is_err());
    }
}
