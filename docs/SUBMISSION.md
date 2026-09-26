# Percolator Vaults: submission

**One-liner.** Keyless perpetual markets with liquidity built in: anyone can open the market for a Pyth price feed on Anatoly Yakovenko's Percolator engine, and a vault program owns it, makes markets on it from one shared pool, and pays its fees to depositors and to whoever opened it.

**Links**
- Live app (Solana devnet): https://percolator-vaults.vercel.app
- Source: https://github.com/Commoneffort/percolator-vaults
- Docs: https://percolator-vaults.vercel.app/#/docs
- Demo video: `docs/demo.mp4` in the repo (upload link to be added)

## Problem

Percolator is a serious perpetual-futures risk engine: cross-margin, bounded price moves, bad debt isolated per asset, and permissionless market listing. But nobody provides liquidity on it. Its own mainnet market has no market maker, so trading means two users co-signing each trade. Listing a market on your own has three problems:

1. The new market is empty.
2. Whoever lists it holds its admin, insurance and oracle keys, so traders have to trust them.
3. Every listing is a separate market, so liquidity for the same asset fragments.

Percolator also settles every trade at the market's mark price. A classic spread-quoting market maker earns nothing there. The income is in the trading fees, which go to whoever operates the asset.

## Solution

A Solana program that turns "operate a Percolator market" into a pooled, permissionless, keyless product:

- **One market per asset.** A vault's address is derived from (market, Pyth feed). The first person to open SOL-PERP creates *the* SOL market, and a second attempt fails on-chain. Every trader and every liquidity provider meets in one place.
- **No keys.** The vault's own address signs the listing, so the vault becomes the market's admin, insurance operator and oracle authority. The program has no instruction that uses those powers. Every market runs on one fixed template, so nobody can open a market set up to fail.
- **Built-in liquidity.** The vault program is also the matcher Percolator calls on every trade. It is the counterparty to all takers, with limits that scale with its value (up to 3× NAV in position, 0.75× per fill), so every deposit deepens the market.
- **Yield.** Trading fees accumulate in the market's insurance. Anyone can harvest the part above a fixed floor into the vault: 90% goes to depositors, and 10% to whoever opened the market, forever, with no other powers (like Hyperliquid's HIP-3 deployer share).
- **Fair exits.** Deposits and withdrawals queue and settle together at one price at the end of each epoch, when the vault is flat. Withdrawals are priced conservatively and deposits pay for unharvested fees, so neither side can take value from the other. If a trader holds a position to block exits, anyone can make the vault close its own position one epoch later.
- **Everything is permissionless.** Rolling epochs, harvesting fees, renewing the matcher approval, closing positions and winding down after market resolution can all be called by anyone. The reference keeper does it automatically.

## What we built

- **On-chain program** (Rust, native Solana, ~3,300 lines including off-chain client builders): vault, matcher, share accounting, market listing, fee harvesting, opener share, liveness backstop, wind-down.
- **38 tests against Percolator's production binary** in LiteSVM, using Percolator's own test harness:
  - property tests for share math;
  - layout and encoding tests checked against Percolator's own types and decoder;
  - end-to-end flows;
  - 16 attack scenarios: forged matcher calls, hijacked LP routing, forged or doubled claims, the first-depositor inflation attack, withdrawal lock-up, and more.
- **Live devnet deployment:** a Percolator build identical to the tested one, the vault program, and live markets on Pyth feeds (SOL, BTC, ETH and more).
- **Web app:**
  - markets directory, and a two-click "open a market" flow;
  - trading, liquidity provision and opener fee claims;
  - live depth ("max trade now"), price chart and activity feed;
  - a leaderboard computed from chain data;
  - detailed docs;
  - a devnet burner wallet so anyone can try it without installing a wallet.
- **Keeper** that discovers every vault and keeps it running.

## Why it matters

Percolator gives Solana an engine for permissionless perps. This gives it a working market structure on top: liquidity that concentrates per asset, markets nobody controls, and a reason for people to supply capital and to open new markets. It is built to go to mainnet against the final Percolator program: the Percolator ID is a compile-time constant, and the remaining steps (audit, burn the upgrade authority, mainnet template values) are documented.

## Honest limits

- Unaudited, and devnet only today.
- The vault is the counterparty to all traders: depositors win if traders lose and pay if traders win. Fees are their compensation.
- Oracle latency can be exploited by traders who see prices before Pyth. It is limited by Percolator's per-slot price cap and our staleness bound, but not eliminated.
- Demo activity on devnet is seeded by four wallets we control, and the site labels them "seed".

## Team

Solo builder: an X1 validator operator and Solana protocol developer, and an upstream contributor to Percolator. They fixed engine bugs (double-charged losses on deposits during a close, liquidation order chosen by the taker, stuck receipts, and more) in PRs #208–#218 and #443–#446 to Percolator's engine and program repositories.
