# Percolator Vaults

Permissionless, adminless liquidity vaults for [Percolator](https://github.com/aeyakovenko/percolator), Anatoly Yakovenko's perpetual-futures risk engine on Solana.

A vault pools deposits, lists its own perpetual market on Percolator, acts as the counterparty to every trade on it, and pays the market's trading fees to its depositors. Nobody holds a key over it: every parameter is fixed when the vault is created, and every maintenance step can be run by anyone.

**Status:** live on Solana devnet, unaudited. Do not use with real funds. Known open issue: fills are immediate at the mark, so traders who see prices before Pyth can front-run the vault; the fix (two-step fills) is designed, not built. See [SECURITY.md](SECURITY.md).

## Why this exists

Percolator is a complete risk engine, but nobody provides liquidity on it yet. Its own mainnet market has no market maker, so the only way to trade is two users co-signing each trade. Percolator also changes the usual economics:

- Every trade settles at the market's mark price. A market maker's quoted price only sizes fees, so an LP earns no spread.
- Trading fees go to the traded asset's insurance fund, which belongs to whoever operates that asset.

So the natural liquidity provider on Percolator is the operator of a market, not a spread-quoting market maker. This program lets a pool of depositors be that operator, with no operator key at all.

## How it works

1. **One market per feed.** An operate-mode vault's address is derived from (market, Pyth feed), so each price feed can have exactly one vault, and so one market and one shared liquidity pool. Whoever opens it only picks the feed: every canonical vault runs on the same fixed template, so nobody can open a market set up to fail. Its caps scale with its own NAV (position up to 3×, each fill up to 0.75×), so an empty vault quotes nothing and every deposit deepens the market. Attach mode, with custom limits, remains for providing liquidity on assets listed by others.
2. **List.** The vault activates a new Percolator asset. Because the vault PDA signs the activation, it becomes the asset's admin, and it names itself as insurance operator, backing authority and oracle authority. It configures the asset's Pyth feed from the fixed parameters, and nothing can reconfigure it afterwards.
3. **Make markets.** The vault owns a Percolator LP portfolio and is also that portfolio's matcher: Percolator calls the vault program on every trade. The matcher enforces a per-fill cap and an inventory cap, and it only accepts calls signed by Percolator's delegate PDA for this vault's portfolio.
4. **Earn.** Takers' fees land in the asset's insurance. `HarvestFees` (anyone can call it) moves everything above the fixed floor into the vault. The floor stays in place to protect traders. Of what is harvested, 90% belongs to the depositors pro rata and 10% is set aside for whoever opened the market (Hyperliquid HIP-3 style), who claims it with `ClaimOpenerFees` and has no other powers.
5. **Settle in epochs.** Deposits and withdrawals are queued, then priced together at one net asset value when the epoch rolls and the LP portfolio is flat. Percolator only allows withdrawals from a flat portfolio, and pricing only at flat points removes every stale-price game.
6. **Keep exits open.** After an epoch ends with requests waiting, the vault only accepts trades that shrink its position. If a trader holds a position on purpose, anyone can call `Unwind` one epoch later, and the vault closes its own position through Percolator's unilateral `RebalanceReduce`.
7. **Retire dead markets.** When a market has had no liquidity providers for three epochs and holds no position, anyone can call `RetireMarket`: its leftover insurance (trading fees nobody else owns) goes to the opener, Percolator retires the asset, and its slot is reused by the next market opened (Percolator fills free slots before growing the market). The same feed can be opened again later.
8. **Wind down.** If the market is resolved, `SettleResolved` (anyone can call it) closes the portfolio through Percolator's resolved path. Queued deposits are refunded and shares redeem pro rata.

## Instructions

| Tag | Instruction | Who | What |
|---|---|---|---|
| 0 | matcher call | Percolator only | Prices a fill during a taker's `TradeCpi` |
| 16 | `InitVault` | anyone | Creates the vault, share mint, buffer, escrow and LP portfolio, and approves the matcher |
| 26 | `ListAsset` | anyone | Operate mode: lists the vault's asset and configures its Pyth feed |
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
| 31 | `RetireMarket` | anyone | Frees the slot of an idle market (no shares, no requests, no position, listed 3+ epochs ago); its leftover insurance goes to the opener and the feed can be opened again |
| 30 | `AcceptGovernance` | current market authority | One-time: hands Percolator's market authority to the program's governor PDA, which is only used by `RetireMarket` |

## Share pricing

- Shares use ERC-4626-style virtual offsets (1,000 virtual shares, 1 virtual asset), so the first-depositor inflation attack loses money.
- At a roll, withdrawals are priced at the **low** NAV: collateral actually held. Deposits are priced at the **high** NAV: plus profit booked but not yet released, plus fees not yet harvested. Neither side can take value from the other.
- Every rounding step goes against the party acting.
- If the vault has lost everything (NAV 0 with shares outstanding), that epoch's deposits are refunded rather than priced.

## Tests

`./test.sh` builds the on-chain program and runs 39 tests. The tests need Percolator's program crate checked out next to this repo (`../percolator-prog`, branch `owl/trunk` of `Commoneffort/percolator-prog`) with its SBF binary built. The integration tests load the **production Percolator SBF binary** into LiteSVM and build markets with Percolator's own test harness.

- **Math and property tests:** withdrawals never exceed NAV, deposit-then-withdraw never profits, incumbents are never diluted, fills never break the inventory cap.
- **Layout tests:** every account offset and every Percolator instruction encoding the vault uses is checked against Percolator's own types and decoder.
- **Flows:** canonical vaults are unique per feed, ignore creator-chosen limits, can be listed by anyone, and scale their caps with NAV. Create → deposit → trade → roll → withdraw. Operate mode: list → trade → harvest fees → exit with profit. Gains and losses reach depositors exactly. Maintenance fees. Market resolution with everyone exiting. Retiring an idle market (governance handoff, idle checks, insurance to the opener) and reopening its feed in the freed slot.
- **Attacks:** forged matcher calls; a foreign LP routing fills through the vault; inventory caps; rolling while not flat; reduce-only enforcement; matcher expiry and permissionless renewal; claims that are early, doubled, stolen or forged; stale tickets; fake buffers; donations; the first-depositor inflation attack; bad parameters, duplicates and a wrong mint; operate-only calls on the wrong mode; double listing; sweeps; a taker holding a position to lock withdrawals.

## Devnet

| | |
|---|---|
| Vault program | `BSync6F8gtJs3Wj4w8L6H3ZtS397w2XoCYGEAJGmeYX` |
| Percolator program (integration branch, program 27d758ed, engine 6de466b; the tested build) | `8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P` |
| Market (test USDC collateral) | `F9zUEE5MZqTLafnFxW2Zp7rKCQ3Wi3Dh1Mvra5eGMq4n` |
| Market authority (vault program governor PDA) | `AQyfoBww2vbLHhjJWmgSmvNH6iQwCjrpdPrDDk45VWsa` |
| SOL-PERP vault | `yzvtftjFGYad1WZYbBTiS7Qj7MVwWAiZhQvUiFuyV5Z` |
| BTC-PERP vault | `CqAfrYYt36kmyK4refBUXv8ymejbRQuPGRh4ayQg9Xyd` |
| ETH-PERP vault | `HQVz3hBLPuuuGwRDa4kPGkniyThvRV88ipFXv2eYUmZV` |

Canonical epochs are 1,500 slots, about 6 minutes at devnet's current ~230 ms slots.

All addresses are in `deploy/devnet.json`. Tooling: `cargo run --example devnet --features devnet -- setup-market | create-vault | deposit | trade | keeper | accept-governance | retire-idle | status`.

## Layout

```
src/            the on-chain program (processor, matcher, share math, state, Percolator bindings, client builders)
tests/          LiteSVM tests against the production Percolator binary
examples/       devnet deployment and keeper
app/            web app (Vite + React + wallet adapter) and the devnet faucet API
pusher/         optional Pyth price pusher (needs a Hermes API key)
```

See [SECURITY.md](SECURITY.md) for the threat model and known limits, the live [docs page](https://percolator-vaults.vercel.app/#/docs) for the full protocol description, [docs/SUBMISSION.md](docs/SUBMISSION.md) for the hackathon write-up and [docs/DEMO_SCRIPT.md](docs/DEMO_SCRIPT.md) for the demo.

Built on Percolator (Apache-2.0).
