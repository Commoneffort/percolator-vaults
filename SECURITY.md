# Security model

## Trust

| Party | Can | Cannot |
|---|---|---|
| Vault creator (opener) | Pick the feed (every other parameter comes from the canonical template); claim 10% of harvested fees | Open a second market for a feed; change any parameter; move funds; pause; upgrade |
| Depositors | Queue deposits and withdrawals; claim; redeem after wind-down | Affect other depositors' prices |
| Traders | Queue trades through the router; move margin in, and out to their own wallet | Trade at a price they know; cancel a queued trade; withdraw while one is queued; trade against the vault any other way |
| Executors (anyone) | Move a vault's mark to a newer verified Pyth price; fill a request once the mark is at its target price (earning its bond); expire requests past their grace period | Choose the price a request fills at; move the mark past a pending request's price; move the mark backwards; fill a request early |
| Anyone | Roll, harvest, convert, renew, unwind, settle, sweep, retire an idle market | Choose amounts or destinations: each of these moves value only into the vault's own buffer or portfolio, or (retiring) the asset's leftover insurance to the opener's claimable balance |
| Percolator market authority (`marketauth`) | Force-shut down any asset (with an exit window); resolve the market | Take the vault's insurance or collateral. On devnet it is the vault program's governor address (`AcceptGovernance`), which the program only uses in `RetireMarket`, so nobody holds these powers |
| Program upgrade authorities | Replace the vault or router program | On devnet this is the deployer. A mainnet deployment must burn both or put them behind a public timelock |
| Pyth | Provide prices, signed by the Wormhole guardians | Nothing else: the router takes only fully verified updates for the vault's feed, forward in time |

## Front-running

Anyone who sees the real price before it reaches the chain (traders on the exchanges Pyth aggregates, Pyth's publishers, readers of Pythnet and Hermes, validators and searchers who see pending transactions) could trade against a market maker that fills immediately at an oracle price. The router removes that:

- A trade is a request whose target time is fixed when it lands: `max(chain clock, current mark time) + 4 s`. It fills only at the first Pyth price published at or after that target (the update with `prev_publish_time < target <= publish_time`). Nobody could know that price when the request landed, and nobody can pick another one.
- The vault's asset is in Percolator's authority-mark mode with the vault as oracle authority. The vault moves the mark only on the router's instruction (`PushMark`), and the router passes only fully verified Pyth updates for the vault's feed, newer than the current mark, and never past a pending request's target.
- The vault's matcher accepts only the fill the router armed for that slot and size (`ArmFill`), so there is no other way to trade against it.
- A request cannot be cancelled; the trader's margin is locked until it fills or expires; it is accepted only with margin for a further 10% price move; an unfilled request expires 30 s after its target with its bond forfeited. A trader cannot back out after seeing the price.
- Filling is permissionless and pays the executor the request's bond, so no keeper can censor a trader, and the executor's identity and timing do not change the fill.

## Invariants the tests enforce

- Only Percolator's matcher delegate for the vault's own portfolio can make the vault quote, and only for a fill the router armed in the same slot for the same size.
- Only the router's authority can move a vault's mark or arm a fill. The router only moves the mark to a fully verified Pyth update for the vault's feed that is newer than the current mark, and to the first update at or after the earliest pending target once that target has passed.
- A request fills only when the mark is the first Pyth price at or after its target and Percolator's effective price equals it; the fill price does not depend on who executes it or when.
- A request cannot be cancelled; the trader cannot withdraw or queue another while it is pending; it expires only after its grace period, and its bond is forfeited.
- Router withdrawals only reach a token account owned by the trader's wallet.
- The vault's inventory never goes past its caps through its own fills. In reduce-only mode it never grows and never crosses zero.
- A roll happens only when the LP portfolio is proven flat. Withdrawn capital is measured, not assumed.
- After every roll and claim, the buffer holds exactly what is owed (`reserved_assets`).
- Withdrawals never pay more than NAV. Deposit-then-withdraw never profits. Deposits never dilute incumbents.
- A ticket can be claimed once, only by its owner, only against the record of its own epoch. Records are created by the roll itself, at a PDA, so they cannot be forged.
- Fee harvesting never takes insurance below the fixed floor while the vault is live.
- Pre-funding a PDA address with lamports cannot block account creation.
- There is at most one vault per (market, feed), and it cannot be squatted: anyone can finish listing a created-but-unlisted vault.
- Vaults never take on more than 3x their NAV in position, or 0.75x per fill, on top of Percolator's margin rules.
- `RetireMarket` only succeeds with no shares outstanding, no queued deposit or withdrawal, no queued trade, a flat LP portfolio and at least three epochs since listing; Percolator additionally refuses unless the asset is empty.
- The opener's fee share is exactly 10% of each harvest, is held in the buffer as a reserved liability (so it never counts toward NAV), and only the opener can claim it.

## Known limits

- **Unaudited.** Neither these programs nor Percolator have been audited.
- **Percolator trust.** The vault inherits Percolator's correctness. Devnet runs our integration branch (program 27d758ed), which includes fixes not yet merged upstream, among them permissionless listing while traders hold open PnL (percolator-prog #447).
- **Chain clock.** A request's target time comes from the chain clock (or the mark time, if later). The 4-second delay assumes the clock does not run more than a few seconds behind real time; Solana's clock is a stake-weighted validator estimate and normally tracks real time within a second or two.
- **A request that cannot fill.** If a request cannot fill (for example a price move larger than the 10% margin buffer, or the vault being unable to take the trade), the mark waits at its target price until the request expires, at most 30 s, before it can move on. It costs the requester the bond; liquidations on that market are delayed by that long.
- **Executors and Pyth access.** Moving marks and filling needs someone to post Pyth updates from Hermes (which requires an API key) and send the transactions. The provided executor does; anyone can run another.
- **Market authority.** Whoever holds `marketauth` can shut down the vault's asset (with Percolator's exit window) or resolve the market; the vault then winds down through `SettleResolved`. On devnet it is the governor address, so nobody can, and permissionless stale resolution is off. On a market we do not govern, this power stays with its authority.
- **Directional risk.** The vault is the counterparty to all traders. If traders win overall, depositors lose. Fees are the compensation.
- **Liveness needs keepers.** Rolls, harvests, cranks, settling out-of-date positions, matcher renewal and retiring idle markets are permissionless, but someone has to send them. The provided keeper does.
- **Unwind price.** `Unwind` closes at Percolator's effective price within its unilateral close capacity. It may take several calls in a stressed market.
