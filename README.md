# Percolator Vaults

Permissionless, adminless liquidity vaults for [Percolator](https://github.com/aeyakovenko/percolator), Anatoly Yakovenko's perpetual-futures risk engine on Solana, with trading that cannot be front-run.

A vault pools deposits, lists its own perpetual market on Percolator, acts as the counterparty to every trade on it, and pays the market's trading fees to its depositors. Every trade goes through a router that fills it at the first Pyth price published four seconds after the trade was requested, a price nobody could know when they traded. Nobody holds a key over any of it: every parameter is fixed when a vault is created, and every maintenance step can be run by anyone.

**Status:** live on Solana devnet, unaudited. Do not use with real funds.

## The problem we solved: trading against a price you already know

A market maker that fills trades immediately at an oracle price can be traded against by anyone who sees the real price first. That is almost every professional trader:

- **Traders watching the exchanges.** Pyth aggregates prices from exchanges and trading firms (Binance, OKX, Coinbase, Jane Street, Jump and others). Anyone connected to those venues sees a move before it reaches Pyth.
- **Pyth publishers.** They know their own quotes before Pyth aggregates them.
- **Anyone reading Pyth off-chain.** A new Pyth price exists on Pythnet and in Hermes (Pyth's price service) before anyone posts it on Solana, and posting is pull-based: the on-chain account is only as fresh as the last person who paid to update it. On devnet we measured Pyth's shared account at 225 seconds old.
- **Validators and searchers.** They see pending transactions, including price updates, and can order their own trades ahead of them.

Before this change the vault filled every trade immediately at Percolator's mark, which followed Pyth. Any of the parties above could buy from it below the real price and sell to it above, and the depositors paid.

### How the router removes it

1. **A trade is a request first.** It lands on chain and gets a target time: the later of the chain clock and the vault's current mark time, plus 4 seconds.
2. **It fills at exactly one price:** the first Pyth price published at or after the target. Pyth's signed update carries its own publish time and the previous one, so the router accepts only the update with `previous publish time < target <= publish time`. Nobody chooses a price: whoever executes the fill, and whenever, the price is the same, and nobody could have known it when the request landed.
3. **The vault's mark only moves through the router, and only forward.** The vault lists its market in Percolator's authority-mark mode with itself as the oracle authority, and it moves the mark only when the router asks, to a fully verified Pyth price that is newer than the current one. The router never moves it past a pending request's target before that request has filled or expired.
4. **There is no other way to trade.** The vault's matcher refuses every fill the router did not arm for that exact slot and size, so a trader calling Percolator directly gets nothing.
5. **No backing out.** A request cannot be cancelled. The trader's margin is locked from request to fill, and the request is only accepted with enough margin to survive a further 10% price move, so a trader cannot make the fill fail after seeing the price. A request nobody fills within 90 seconds of its target expires and its bond is forfeited; nothing is traded.
6. **Nobody can censor it.** Filling is permissionless: any wallet, the trader included, can execute any request once the mark is at its price, and earns the request's bond for doing so. Each trader's Percolator account is owned by a program address derived from their wallet; only the router can trade it, and withdrawals can only go back to that wallet.

What is left: the design assumes the chain clock does not run more than a few seconds behind real time (the 4-second delay is the margin), and a request that cannot fill (for example after a price move larger than the 10% margin buffer) holds the mark at most 90 seconds before it expires, at the cost of its bond.

## Why this exists

Percolator is a complete risk engine, but nobody provides liquidity on it yet. Its own mainnet market has no market maker, so the only way to trade is two users co-signing each trade. Percolator also changes the usual economics:

- Every trade settles at the market's mark price. A market maker's quoted price only sizes fees, so an LP earns no spread.
- Trading fees go to the traded asset's insurance fund, which belongs to whoever operates that asset.

So the natural liquidity provider on Percolator is the operator of a market, not a spread-quoting market maker. This program lets a pool of depositors be that operator, with no operator key at all.

## How it works

1. **One market per feed.** A vault's address is derived from (market, Pyth feed), so each price feed can have exactly one vault, and so one market and one shared liquidity pool. Whoever opens it only picks the feed: every vault runs on the same fixed template, so nobody can open a market set up to fail. Its caps scale with its own NAV (position up to 3×, each fill up to 0.75×), so an empty vault quotes nothing and every deposit deepens the market.
2. **List.** The vault activates a new Percolator asset at a fresh, verified Pyth price. Because the vault's address signs the activation, it becomes the asset's admin, insurance operator, backing authority and oracle authority. The program has no instruction that uses those powers except moving the mark on the router's request.
3. **Trade through the router.** Takers queue requests; executors move the mark to the verified Pyth price at each request's target and fill it (see above). The vault is the counterparty, within its caps.
4. **Earn.** Takers' fees land in the asset's insurance. `HarvestFees` (anyone can call it) moves everything above the fixed floor into the vault. The floor stays in place to protect traders. Of what is harvested, 90% belongs to the depositors pro rata and 10% is set aside for whoever opened the market (Hyperliquid HIP-3 style), who claims it with `ClaimOpenerFees` and has no other powers.
5. **Settle in epochs, without stopping trading.** Deposits and withdrawals are queued, then priced together at one net asset value when the epoch ends. A flat vault's value does not depend on the price, so anyone can roll it. A vault holding a position is settled through the router instead: anyone queues the settlement, and it executes at the first Pyth price published after its target time, like a trade, so nobody can choose the price it is valued at. Capital stays in Percolator and is counted at Percolator's figure; withdrawals are paid from cash, of which the vault keeps 10% of its value outside Percolator.
6. **Keep exits open.** If an epoch's withdrawals are larger than the vault's cash, they can only be paid once its position is closed (Percolator pays out only from a flat account). Anyone can then switch the vault to closing-only (`RequireFlat`): it only accepts trades that shrink its position. If a trader keeps a position open on purpose, anyone can call `Unwind` one epoch later, and the vault closes its own position through Percolator's unilateral `RebalanceReduce`. That deleverages the traders on the other side, and Percolator then takes no new position on the market until every position on it is closed. The vault cannot take the other side of a close there, so the router closes the remaining positions out (`RequestClose`: anyone can queue it for any trader, and it executes at the first Pyth price after its target like any request); the market then resets and reopens.
7. **Liquidate before Percolator has to.** A trader whose equity falls below 12.5% of their positions' notional can be liquidated through the router by anyone: the request executes at the first Pyth price after its target, and closes the position against the vault if the account is still below that level at that price. Percolator's own liquidation (at its 10% maintenance margin, on any crank) remains the backstop; it reduces the position unilaterally, which makes the market close-only until it is closed out as above.
7. **Retire dead markets.** When a market has had no liquidity providers for three epochs, holds no position and has no trade queued, anyone can call `RetireMarket`: its leftover insurance (trading fees nobody else owns) goes to the opener, Percolator retires the asset, and its slot is reused by the next market opened. The same feed can be opened again later.
8. **Wind down.** If the market is resolved, `SettleResolved` (anyone can call it) closes the portfolio through Percolator's resolved path. Queued deposits are refunded and shares redeem pro rata.

## Instructions

### Vault program

| Tag | Instruction | Who | What |
|---|---|---|---|
| 0 | matcher call | Percolator only | Sizes a fill the router armed, during its `TradeCpi` |
| 16 | `InitVault` | anyone | Creates the vault, share mint, buffer, escrow and LP portfolio, and approves the matcher |
| 26 | `ListAsset` | anyone | Lists the vault's asset in authority-mark mode at a fresh, verified Pyth price |
| 17 / 18 | `RequestDeposit` / `RequestWithdraw` | depositor | Queues a request for the current epoch |
| 19 | `RollEpoch` | anyone | Settles the epoch while the portfolio is flat, then redeploys capital |
| 20 | `Claim` | depositor | Collects the shares or collateral from a settled epoch |
| 27 | `HarvestFees` | anyone | Moves insurance above the floor into the vault |
| 22 | `ConvertPnl` | anyone | Turns released profit into withdrawable capital |
| 21 | `RefreshMatcher` | anyone | Renews Percolator's time-limited matcher approval |
| 28 | `Unwind` | anyone | Liveness backstop, once `RequireFlat` applies and a further epoch has passed: the vault closes its own position |
| 34 | `SyncInventory` | anyone | Re-reads the vault's position from Percolator after it changed outside a fill (a liquidation or close-out on the other side) |
| 35 | `RequireFlat` | anyone | Switches to closing-only when an overdue epoch's withdrawals exceed the vault's cash |
| 29 | `ClaimOpenerFees` | market opener | Collects the opener's 10% share of harvested fees |
| 23 / 24 | `SettleResolved` / `RedeemTerminal` | anyone / holder | Wind-down after market resolution |
| 25 | `Sweep` | anyone | Moves stray vault-owned collateral into the buffer |
| 31 | `RetireMarket` | anyone | Frees the slot of an idle market (no shares, no requests, no position, nothing queued, listed 3+ epochs ago); leftover insurance goes to the opener |
| 30 | `AcceptGovernance` | current market authority | One-time: hands Percolator's market authority to the program's governor address, which is only used by `RetireMarket` |
| 32 | `PushMark` | router only | Moves the asset's mark (the router passes only verified Pyth prices) |
| 33 | `ArmFill` | router only | Arms the one fill the matcher accepts in this slot |

### Router program

| Tag | Instruction | Who | What |
|---|---|---|---|
| 0 | `OpenAccount` | trader | Creates the trader's router account, its Percolator portfolio and collateral account |
| 1 / 2 | `Deposit` / `Withdraw` | trader | Margin in; margin out to the trader's own wallet (not while a request is pending) |
| 3 | `OpenBook` | anyone | Creates a vault's queue of requests |
| 4 | `Request` | trader | Queues a trade; sets its target time; checks margin with a 10% stress buffer; takes a bond |
| 5 | `Advance` | anyone | Moves a vault's mark to a newer verified Pyth price, never past a pending target |
| 6 | `Fill` | anyone | Fills a request at the mark once the mark is its target price; pays the executor the bond |
| 7 | `Expire` | anyone | Removes a request unfilled 90 s after its target; the bond is forfeited |
| 8 | `RequestClose` | anyone, for any trader | Queues a forced close: a close-out on a close-only market (unilateral reduce at the mark), or the liquidation of an account below 12.5% (closed against the vault if still below at the fill price) |
| 9 | `RequestSettle` | anyone | Queues the settlement of a vault's epoch at the first Pyth price after its target |
| 10 | `Settle` | anyone | Rolls the vault's epoch at that price, with its position open |

## Share pricing

- Shares use ERC-4626-style virtual offsets (1,000 virtual shares, 1 virtual asset), so the first-depositor inflation attack loses money.
- At a roll, withdrawals are priced at the **low** NAV: collateral actually held. Deposits are priced at the **high** NAV: plus profit booked but not yet released, plus fees not yet harvested. Neither side can take value from the other.
- Every rounding step goes against the party acting.
- If the vault has lost everything (NAV 0 with shares outstanding), that epoch's deposits are refunded rather than priced.

## Tests

`./test.sh` builds both on-chain programs and runs 59 tests. The tests need Percolator's program crate checked out next to this repo (`../percolator-prog`, branch `owl/trunk` of `Commoneffort/percolator-prog`) with its SBF binary built. The integration tests load the **production Percolator SBF binary** into LiteSVM and build markets with Percolator's own test harness; every trade in them goes through the router.

- **Front-running:** a trader who knows the price in advance ends flat, not ahead; the fill is exactly the first Pyth price at or after the target; a later update, or one that is not the first after the target, cannot move the mark; nobody can push a newer price over a pending request; the executor's identity and timing do not change the fill; a fill waits until Percolator's price has reached the mark; a direct trade against the vault is refused; only the router can move the mark or arm a fill; forged, unverified and wrong-feed Pyth accounts are refused.
- **Requests:** they cannot be cancelled or withdrawn from, and expire only after the grace period with the bond forfeited; margin must survive a stressed move, except for a trade that only reduces a position; withdrawals only reach the owner's wallet; a market with a queued request cannot be retired.
- **Math and property tests:** withdrawals never exceed NAV, deposit-then-withdraw never profits, incumbents are never diluted, fills never break the caps.
- **Layout tests:** every account offset and every Percolator instruction encoding the programs use is checked against Percolator's own types and decoder.
- **Flows:** canonical vaults are unique per feed, ignore creator-chosen limits, can be listed by anyone, and scale their caps with NAV. Create → deposit → trade → roll → withdraw. List → trade → harvest fees → exit with profit. Gains and losses reach depositors exactly. Maintenance fees. Market resolution with everyone exiting. Retiring an idle market and reopening its feed in the freed slot.
- **Attacks:** forged matcher calls; a foreign LP routing fills through the vault; inventory caps; rolling while not flat; reduce-only enforcement; matcher expiry and permissionless renewal; claims that are early, doubled, stolen or forged; stale tickets; fake buffers; donations; the first-depositor inflation attack; attach mode and bad modes refused; duplicates and a wrong mint; double listing; sweeps; a taker holding a position to lock withdrawals.

## Devnet

| | |
|---|---|
| Vault program | `BSync6F8gtJs3Wj4w8L6H3ZtS397w2XoCYGEAJGmeYX` |
| Router program | `DkK9TSMpVXLq26HeqxTXLysXyRKDYHTKU94SLFDWgjw3` |
| Percolator program (integration branch, program 27d758ed, engine 6de466b; the tested build) | `8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P` |
| Market (test USDC collateral) | `8ZMjDzhAgcV7zidvkRXM257Dp2HmzeNz7jpbTFvRE1Ls` |
| Market authority (vault program governor address) | `78Hr1UZdw5zPX393dDNK1WnBBmjYWfe4WN3ZurvCnAkR` |
| SOL-PERP vault | `GJ5JgntYZrsW4S2GpAL53ixg5gPviFThQjk9NtwmgAat` |
| BTC-PERP vault | `AjBANbnVyHpEFPpPg3Rq11UJPGjXbxLruiP68PaUBL3F` |
| ETH-PERP vault | `EUEzFumrPr8jzRuGfQtcdY3mLLWpBiL5yspRcQShgGPD` |

Canonical epochs are 1,500 slots, about 6 minutes at devnet's current ~230 ms slots.

All addresses are in `deploy/devnet.json`. Tooling: `cargo run --example devnet --features devnet -- setup-market | create-vault | keeper | accept-governance | retire-idle | status`.

### Running the executor on Pyth's free plan

The executor (`app/scripts/executor.ts`) moves marks and fills requests. It needs a Pyth API key (Pyth's free plan is enough), read from `HERMES_API_KEY` or `~/.config/solana/percolator-test/hermes-key`; the endpoint is `https://pyth.dourolabs.app/hermes` (override with `HERMES_URL`).

- **Feeds.** The free plan serves SOL, BTC, ETH, PYTH, DOGE, HYPE, XAU and EUR, so those are the markets the app offers (JUP, WIF, RAY, JTO, BONK and TRUMP are refused on the free plan).
- **Usage.** Keeping marks fresh costs nothing: the executor uses Pyth's own sponsored on-chain feed accounts (updated at least every minute, or on a 0.5% move). Hermes is called only when a trade needs its exact target price: one request per target time, shared by every trade queued for it, plus one batched request every few minutes to refresh the feed accounts of markets not opened yet. That is a few Hermes requests per trade, far below the documented limit of 30 requests per 10 seconds. Pyth does not publish a monthly quota for the free plan; the executor backs off for a minute whenever Hermes answers 429.
- **RPC.** It fits within the public devnet RPC's limits: one batched account read every 3 seconds (every second while a trade is queued), requests sent one at a time with a pause on HTTP 429, and confirmations by polling rather than websockets.
- **Cranks.** Percolator takes new risk on an asset only when that asset is accrued to the current slot and every position on it is settled, and it settles a trader's positions only once every asset that trader holds is current. The executor puts exactly those cranks in front of each fill (`crankPlan` in `app/src/chain.ts`). How many are needed depends on the slot the transaction lands in, and a crank with nothing to do fails, so it sends two versions of the fill one crank apart: only the one that matches can succeed, and the request fills once. It also finalizes Percolator's side reset when the last position on one side of a market closes, without which that market would refuse new positions. Between trades it keeps every market within about 50 slots of the chain and settles out-of-date positions.

`RPC=https://api.devnet.solana.com npx tsx scripts/executor.ts` (from `app/`).

## Layout

```
src/            the vault program (processor, matcher, share math, state, Pyth reader, Percolator bindings, client builders)
router/         the router program (requests, marks, fills) and its client builders
tests/          LiteSVM tests against the production Percolator binary
examples/       devnet deployment and keeper
app/            web app (Vite + React + wallet adapter), the devnet faucet API, and scripts (executor, seed)
pusher/         standalone Pyth price pusher for shared feed accounts (needs a Pyth API key)
```

See [SECURITY.md](SECURITY.md) for the threat model and known limits, the live [docs page](https://percolator-vaults.vercel.app/#/docs) for the full protocol description, [docs/SUBMISSION.md](docs/SUBMISSION.md) for the hackathon write-up and [docs/DEMO_SCRIPT.md](docs/DEMO_SCRIPT.md) for the demo.

Built on Percolator (Apache-2.0).
