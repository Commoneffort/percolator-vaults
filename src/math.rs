//! Share pricing and matcher pricing. Pure functions, no Solana types, fully unit-tested.
//!
//! Every rounding step goes against the party acting (the depositor, the withdrawer or the
//! taker), so the remaining holders can never be diluted by rounding.

use crate::error::VaultError;

/// Virtual shares and assets (the OpenZeppelin ERC-4626 offset). They make the first-depositor
/// "donation inflation" attack cost more than it can steal, and keep prices defined at zero.
pub const VIRTUAL_SHARES: u128 = 1_000;
pub const VIRTUAL_ASSETS: u128 = 1;
pub const BPS: u128 = 10_000;

/// Assets paid for burning `shares`, priced at `nav` over `supply`. Rounded down and never more
/// than `nav`.
pub fn assets_for_shares(shares: u64, nav: u64, supply: u64) -> Result<u64, VaultError> {
    if shares > supply {
        return Err(VaultError::Overflow);
    }
    let num = (shares as u128)
        .checked_mul(nav as u128 + VIRTUAL_ASSETS)
        .ok_or(VaultError::Overflow)?;
    let out = num / (supply as u128 + VIRTUAL_SHARES);
    Ok(core::cmp::min(out, nav as u128) as u64)
}

/// Shares minted for depositing `assets`, priced at `nav` over `supply`. Rounded down.
/// Returns `None` when the result would not fit in a u64 supply: the caller refunds instead.
pub fn shares_for_assets(assets: u64, nav: u64, supply: u64) -> Option<u64> {
    let num = (assets as u128).checked_mul(supply as u128 + VIRTUAL_SHARES)?;
    let out = num / (nav as u128 + VIRTUAL_ASSETS);
    let out = u64::try_from(out).ok()?;
    out.checked_add(supply)?;
    Some(out)
}

/// Pro-rata share of `total` for `part` out of `whole`, rounded down.
pub fn pro_rata(part: u64, total: u64, whole: u64) -> Result<u64, VaultError> {
    if whole == 0 {
        return Ok(0);
    }
    if part > whole {
        return Err(VaultError::Overflow);
    }
    Ok(((part as u128 * total as u128) / whole as u128) as u64)
}

/// Execution price quoted to a taker: above the oracle when the taker buys, below when it sells,
/// always rounded against the taker. `spread_bps` must be below 10,000.
pub fn quote_price(oracle_e6: u64, taker_buys: bool, spread_bps: u16) -> Result<u64, VaultError> {
    let s = spread_bps as u128;
    if s >= BPS || oracle_e6 == 0 {
        return Err(VaultError::InvalidParams);
    }
    let o = oracle_e6 as u128;
    let p = if taker_buys {
        (o * (BPS + s)).div_ceil(BPS)
    } else {
        (o * (BPS - s)) / BPS
    };
    if p == 0 {
        return Err(VaultError::InvalidParams);
    }
    u64::try_from(p).map_err(|_| VaultError::Overflow)
}

/// How much of a taker request the vault fills, as a non-negative size.
///
/// `inventory` is the vault's position (positive = long). `lp_delta_sign` is the direction the
/// fill moves the vault: +1 when the taker sells (the vault buys), -1 when the taker buys.
/// In reduce-only mode only fills that move the inventory toward zero, without crossing it, are
/// allowed. Otherwise the resulting inventory is kept within `max_inventory_abs`.
pub fn allowed_fill(
    requested_abs: u128,
    max_fill_abs: u128,
    inventory: i128,
    lp_delta_sign: i8,
    max_inventory_abs: u128,
    reduce_only: bool,
) -> u128 {
    let want = core::cmp::min(requested_abs, max_fill_abs);
    let inv_abs = inventory.unsigned_abs();
    let reducing = inventory != 0 && (inventory > 0) != (lp_delta_sign > 0);
    if reduce_only {
        return if reducing { core::cmp::min(want, inv_abs) } else { 0 };
    }
    // Room before hitting the cap on the side the fill moves toward.
    let room = if reducing {
        inv_abs.saturating_add(max_inventory_abs)
    } else {
        max_inventory_abs.saturating_sub(inv_abs)
    };
    core::cmp::min(want, room)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn first_deposit_and_exit_round_trip() {
        let minted = shares_for_assets(1_000_000, 0, 0).unwrap();
        assert_eq!(minted, 1_000_000_000);
        let back = assets_for_shares(minted, 1_000_000, minted).unwrap();
        assert!(back <= 1_000_000 && back >= 999_999);
    }

    #[test]
    fn donation_attack_is_unprofitable() {
        // Attacker deposits 1 atom, then donates D to inflate the price; a victim deposits V.
        let attacker_shares = shares_for_assets(1, 0, 0).unwrap();
        let donation = 1_000_000u64;
        let nav = 1 + donation;
        let victim = 500_000u64;
        let victim_shares = shares_for_assets(victim, nav, attacker_shares).unwrap();
        assert!(victim_shares > 0);
        let supply = attacker_shares + victim_shares;
        let nav2 = nav + victim;
        let attacker_out = assets_for_shares(attacker_shares, nav2, supply).unwrap();
        assert!(attacker_out < 1 + donation, "attacker must lose on the donation");
    }

    #[test]
    fn quotes_round_against_taker() {
        assert_eq!(quote_price(1_000_000, true, 50).unwrap(), 1_005_000);
        assert_eq!(quote_price(1_000_000, false, 50).unwrap(), 995_000);
        assert_eq!(quote_price(3, true, 1).unwrap(), 4);
        assert_eq!(quote_price(3, false, 1).unwrap(), 2);
        assert!(quote_price(1, false, 5_000).is_err());
        assert!(quote_price(1_000_000, true, 10_000).is_err());
    }

    #[test]
    fn fills_respect_caps_and_reduce_only() {
        // Flat vault, cap 100, taker buys 150 (vault sells): clamp to cap.
        assert_eq!(allowed_fill(150, 1_000, 0, -1, 100, false), 100);
        // Vault long 80, taker buys 500 (vault sells, reducing): room is 80 + 100.
        assert_eq!(allowed_fill(500, 1_000, 80, -1, 100, false), 180);
        // Reduce-only never crosses zero and never grows.
        assert_eq!(allowed_fill(500, 1_000, 80, -1, 100, true), 80);
        assert_eq!(allowed_fill(500, 1_000, 80, 1, 100, true), 0);
        assert_eq!(allowed_fill(500, 1_000, 0, 1, 100, true), 0);
        // Per-fill cap.
        assert_eq!(allowed_fill(500, 30, 0, 1, 100, false), 30);
    }

    proptest! {
        #[test]
        fn withdraw_never_exceeds_nav(shares in 0u64.., nav in 0u64.., supply in 0u64..) {
            prop_assume!(shares <= supply);
            let out = assets_for_shares(shares, nav, supply).unwrap();
            prop_assert!(out <= nav);
        }

        #[test]
        fn deposit_then_withdraw_never_profits(
            d in 1u64..1_000_000_000_000, nav in 0u64..1_000_000_000_000, supply in 0u64..1_000_000_000_000
        ) {
            if let Some(m) = shares_for_assets(d, nav, supply) {
                let out = assets_for_shares(m, nav + d, supply + m).unwrap();
                prop_assert!(out <= d);
            }
        }

        #[test]
        fn incumbents_never_diluted_by_deposit(
            d in 1u64..1_000_000_000_000, nav in 1u64..1_000_000_000_000, supply in 1u64..1_000_000_000_000
        ) {
            // Value per share (with offsets) must not fall when a deposit is minted.
            if let Some(m) = shares_for_assets(d, nav, supply) {
                let before = (nav as u128 + VIRTUAL_ASSETS) * (supply as u128 + m as u128 + VIRTUAL_SHARES);
                let after = (nav as u128 + d as u128 + VIRTUAL_ASSETS) * (supply as u128 + VIRTUAL_SHARES);
                prop_assert!(after >= before);
            }
        }

        #[test]
        fn fill_never_breaks_inventory_cap(
            req in 0u128..1u128 << 80, maxf in 0u128..1u128 << 80, inv in -(1i128 << 70)..(1i128 << 70),
            buy in any::<bool>(), cap in 0u128..1u128 << 70, ro in any::<bool>()
        ) {
            let sign: i8 = if buy { 1 } else { -1 };
            let f = allowed_fill(req, maxf, inv, sign, cap, ro);
            prop_assert!(f <= req && f <= maxf);
            let new = inv + sign as i128 * f as i128;
            if ro {
                prop_assert!(new.unsigned_abs() <= inv.unsigned_abs());
                prop_assert!(new == 0 || new.signum() == inv.signum());
            } else if inv.unsigned_abs() <= cap {
                prop_assert!(new.unsigned_abs() <= cap);
            } else {
                prop_assert!(new.unsigned_abs() <= inv.unsigned_abs() || new.unsigned_abs() <= cap);
            }
        }
    }
}
