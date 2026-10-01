# Percolator Vaults: submission

**One-liner.** Keyless perpetual markets with liquidity built in, that nobody can front-run: anyone can open the market for a Pyth price feed on Anatoly Yakovenko's Percolator engine, a vault program owns it and makes markets on it from one shared pool, and every trade fills at the first Pyth price published four seconds after it was requested, a price nobody could know when they traded.

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

And a pool that fills trades immediately at an oracle price gets front-run. Pyth aggregates prices from exchanges and trading firms, so anyone watching those venues, Pyth's own publishers, anyone reading Pyth's off-chain feed before it is posted on Solana (we measured the shared devnet account at 225 seconds old), and validators or searchers who see pending price updates, can all trade against it at a price they already know. Depositors pay for every such trade.

## Solution

A Solana program that turns "operate a Percolator market" into a pooled, permissionless, keyless product:

- **One market per asset.** A vault's address is derived from (market, Pyth feed). The first person to open SOL-PERP creates *the* SOL market, and a second attempt fails on-chain. Every trader and every liquidity provider meets in one place.
- **No keys.** The vault's own address signs the listing, so the vault becomes the market's admin, insurance operator and oracle authority. The program has no instruction that uses those powers. Every market runs on one fixed template, so nobody can open a market set up to fail.
- **No front-running.** Every trade is a request that fills at exactly one price: the first Pyth price published at or after its target time (the moment it landed, plus 4 seconds). A router program, the only way to trade, moves the vault's mark only to verified Pyth prices, forward in time, never past a pending request's price, and fills requests permissionlessly, so neither the executor nor its timing changes the price. Requests cannot be cancelled and margin is locked until they fill, so nobody can back out after seeing the price. The vault's matcher refuses any fill the router did not arm.
- **Built-in liquidity.** The vault program is also the matcher Percolator calls on every trade. It is the counterparty to all takers, with limits that scale with its value (up to 3× NAV in position, 0.75× per fill), so every deposit deepens the market.
- **Yield.** Trading fees accumulate in the market's insurance. Anyone can harvest the part above a fixed floor into the vault: 90% goes to depositors, and 10% to whoever opened the market, forever, with no other powers (like Hyperliquid's HIP-3 deployer share).
- **Fair exits.** Deposits and withdrawals queue and settle together at one price at the end of each epoch, when the vault is flat. Withdrawals are priced conservatively and deposits pay for unharvested fees, so neither side can take value from the other. If a trader holds a position to block exits, anyone can make the vault close its own position one epoch later.
- **Dead markets clean up after themselves.** A market with no liquidity providers for three epochs and no position can be retired by anyone. Its leftover insurance goes to the opener, and its slot is reused by the next market. The program holds Percolator's market-level authority through a program address it uses for nothing else, so the market has no admin key at all.
- **Everything is permissionless.** Filling trades, moving marks, rolling epochs, harvesting fees, renewing the matcher approval, closing positions, retiring idle markets and winding down after market resolution can all be called by anyone. The reference keeper and executor do it automatically.

## What we built

- **Two on-chain programs** (Rust, native Solana, ~4,500 lines including off-chain client builders): the vault (matcher, share accounting, market listing, fee harvesting, opener share, liveness backstop, wind-down, retirement) and the router (trading accounts, requests, verified Pyth marks, fills, expiry).
- **51 tests against Percolator's production binary** in LiteSVM, using Percolator's own test harness, every trade through the router:
  - front-running attempts: trading on a known price, skipping or reordering Pyth updates, executor timing, direct trades, forged and unverified Pyth accounts, cancelling or withdrawing from a queued trade;
  - property tests for share math;
  - layout and encoding tests checked against Percolator's own types and decoder;
  - end-to-end flows, including retiring an idle market and reopening its feed;
  - 15 attack scenarios: forged matcher calls, hijacked LP routing, forged or doubled claims, the first-depositor inflation attack, withdrawal lock-up, and more.
- **Live devnet deployment:** a Percolator build identical to the tested one, the vault and router programs, and markets on Pyth feeds (SOL, BTC, ETH open; PYTH, DOGE, HYPE, gold and EUR can be opened by anyone). Trades are queued and filled through the router at their target Pyth price; a trade sent straight to a vault is refused on chain.
- **Web app:**
  - markets directory, and a two-click "open a market" flow;
  - trading through the router (queue a trade, see its target time and fill), liquidity provision and opener fee claims;
  - live depth ("max trade now"), price chart and activity feed;
  - a leaderboard computed from chain data;
  - detailed docs;
  - a devnet burner wallet so anyone can try it without installing a wallet.
- **Keeper** that discovers every vault and keeps it running: rolls, harvests, matcher renewal, retiring idle markets.
- **Executor** that anyone can run on Pyth's free plan: fetches each trade's target-time Pyth update from Hermes (a few requests per trade), posts it, fills the trade at it with the cranks Percolator needs first (accruing every asset involved and settling out-of-date positions), keeps marks and accrual fresh between trades from Pyth's free sponsored feed accounts, and expires late requests. It runs on the public devnet RPC.
- **Percolator fixes found while building it:** a Percolator bug blocked permissionless listing whenever any trader held open PnL; fixed on our integration branch, deployed on devnet, and submitted upstream (percolator-prog #447).

## Why it matters

Percolator gives Solana an engine for permissionless perps. This gives it a working market structure on top: liquidity that concentrates per asset, markets nobody controls, and a reason for people to supply capital and to open new markets. It is built to go to mainnet against the final Percolator program: the Percolator ID is a compile-time constant, and the remaining steps (audit, burn both upgrade authorities, mainnet template values, redundant executors with Pyth API access) are documented.

## Honest limits

- Unaudited, and devnet only today.
- The vault is the counterparty to all traders: depositors win if traders lose and pay if traders win. Fees are their compensation.
- Trades take a few seconds: that delay is what makes the fill price one nobody could know in advance. Its protection assumes the chain clock is not more than a few seconds behind real time.
- A request that cannot fill (for example after a price move larger than its 10% margin buffer) holds the market's mark at its price for at most 90 seconds before it expires, at the cost of its bond.
- Every price move leaves each open position on that asset out of date until it is cranked, and Percolator takes no new position on the asset until all of them are; the executor does this in front of each fill, so a fill costs more the more positions a market has.
- Executors need a Pyth API key (the free plan covers the markets offered) to fetch each trade's target-time update; anyone can run one, and if nobody does, requests expire and nothing trades.
- Demo activity on devnet is seeded by four wallets we control, and the site labels them "seed".

## Team

Solo builder: an X1 validator operator and Solana protocol developer, and an upstream contributor to Percolator. They fixed engine bugs (double-charged losses on deposits during a close, liquidation order chosen by the taker, stuck receipts, and more) in PRs #208–#218 and #443–#447 to Percolator's engine and program repositories, and reported further findings as issues (engine #219, program #448–#450).
