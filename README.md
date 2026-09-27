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
5. **No backing out.** A request cannot be cancelled. The trader's margin is locked from request to fill, and the request is only accepted with enough margin to survive a further 10% price move, so a trader cannot make the fill fail after seeing the price. A request nobody fills within 30 seconds of its target expires and its bond is forfeited; nothing is traded.
6. **Nobody can censor it.** Filling is permissionless: any wallet, the trader included, can execute any request once the mark is at its price, and earns the request's bond for doing so. Each trader's Percolator account is owned by a program address derived from their wallet; only the router can trade it, and withdrawals can only go back to that wallet.

What is left: the design assumes the chain clock does not run more than a few seconds behind real time (the 4-second delay is the margin), and a request that cannot fill (for example after a price move larger than the 10% margin buffer) holds the mark at most 30 seconds before it expires, at the cost of its bond.

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
5. **Settle in epochs.** Deposits and withdrawals are queued, then priced together at one net asset value when the epoch rolls and the LP portfolio is flat. Percolator only allows withdrawals from a flat portfolio, and pricing only at flat points removes every stale-price game.
6. **Keep exits open.** After an epoch ends with requests waiting, the vault only accepts trades that shrink its position. If a trader holds a position on purpose, anyone can call `Unwind` one epoch later, and the vault closes its own position through Percolator's unilateral `RebalanceReduce`.
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
| 28 | `Unwind` | anyone | Liveness backstop: the vault closes its own position |
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
| 7 | `Expire` | anyone | Removes a request unfilled 30 s after its target; the bond is forfeited |

## Share pricing

- Shares use ERC-4626-style virtual offsets (1,000 virtual shares, 1 virtual asset), so the first-depositor inflation attack loses money.
- At a roll, withdrawals are priced at the **low** NAV: collateral actually held. Deposits are priced at the **high** NAV: plus profit booked but not yet released, plus fees not yet harvested. Neither side can take value from the other.
- Every rounding step goes against the party acting.
- If the vault has lost everything (NAV 0 with shares outstanding), that epoch's deposits are refunded rather than priced.

## Tests

`./test.sh` builds both on-chain programs and runs 51 tests. The tests need Percolator's program crate checked out next to this repo (`../percolator-prog`, branch `owl/trunk` of `Commoneffort/percolator-prog`) with its SBF binary built. The integration tests load the **production Percolator SBF binary** into LiteSVM and build markets with Percolator's own test harness; every trade in them goes through the router.

- **Front-running:** a trader who knows the price in advance ends flat, not ahead; the fill is exactly the first Pyth price at or after the target; a later update, or one that is not the first after the target, cannot move the mark; nobody can push a newer price over a pending request; the executor's identity and timing do not change the fill; a fill waits until Percolator's price has reached the mark; a direct trade against the vault is refused; only the router can move the mark or arm a fill; forged, unverified and wrong-feed Pyth accounts are refused.
- **Requests:** they cannot be cancelled or withdrawn from, and expire only after the grace period with the bond forfeited; margin must survive a stressed move; withdrawals only reach the owner's wallet; a market with a queued request cannot be retired.
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

All addresses are in `deploy/devnet.json`. Tooling: `cargo run --example devnet --features devnet -- setup-market | create-vault | keeper | accept-governance | retire-idle | status`. The executor (`app/scripts/executor.ts`) moves marks and fills requests; it needs a Hermes API key (`HERMES_URL`, `HERMES_API_KEY`).

## Layout

```
src/            the vault program (processor, matcher, share math, state, Pyth reader, Percolator bindings, client builders)
router/         the router program (requests, marks, fills) and its client builders
tests/          LiteSVM tests against the production Percolator binary
examples/       devnet deployment and keeper
app/            web app (Vite + React + wallet adapter), the devnet faucet API, and scripts (executor, seed)
pusher/         Pyth price pusher for shared feed accounts (needs a Hermes API key)
```

See [SECURITY.md](SECURITY.md) for the threat model and known limits, the live [docs page](https://percolator-vaults.vercel.app/#/docs) for the full protocol description, [docs/SUBMISSION.md](docs/SUBMISSION.md) for the hackathon write-up and [docs/DEMO_SCRIPT.md](docs/DEMO_SCRIPT.md) for the demo.

Built on Percolator (Apache-2.0).
