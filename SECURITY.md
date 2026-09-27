# Security model

## Trust

| Party | Can | Cannot |
|---|---|---|
| Vault creator | Operate mode: pick the feed (all other parameters come from the canonical template). Attach mode: choose the parameters once | Open a second market for a feed; change any parameter; move funds; pause; upgrade |
| Depositors | Queue deposits and withdrawals; claim; redeem after wind-down | Affect other depositors' prices |
| Anyone | Roll, harvest, convert, renew, unwind, settle, sweep, retire an idle market | Choose amounts or destinations: each of these moves value only into the vault's own buffer or its own portfolio, or (retiring) the asset's leftover insurance to the opener's claimable balance |
| Takers | Trade against the vault within its caps | Call the matcher directly; route fills through another LP; lock withdrawals |
| Percolator market authority (`marketauth`) | Force-shut down any asset (with an exit window); resolve the market | Take the vault's insurance or collateral. On devnet it is the program's governor PDA (`AcceptGovernance`), which the program only uses in `RetireMarket`, so nobody holds these powers |
| Program upgrade authority | Replace the program | (On devnet this is the deployer. A mainnet deployment must burn it or put it behind a public timelock.) |
| Pyth | Provide the price | Nothing else: staleness is bounded (at most 300 s) and Percolator caps the price move per slot |

## Invariants the tests enforce

- Only Percolator's matcher delegate PDA for the vault's own portfolio can make the vault quote. Only Percolator can sign for that PDA.
- The vault's inventory never goes past `max_inventory_abs` through its own fills. In reduce-only mode it never grows and never crosses zero.
- A roll happens only when the LP portfolio is proven flat. Withdrawn capital is measured, not assumed.
- After every roll and claim, the buffer holds exactly what is owed (`reserved_assets`).
- Withdrawals never pay more than NAV. Deposit-then-withdraw never profits. Deposits never dilute incumbents.
- A ticket can be claimed once, only by its owner, only against the record of its own epoch. Records are created by the roll itself, at a PDA, so they cannot be forged.
- Fee harvesting never takes insurance below the fixed floor while the vault is live.
- Pre-funding a PDA address with lamports cannot block account creation.
- There is at most one operate-mode vault per (market, feed), and it cannot be squatted: anyone can finish listing a created-but-unlisted vault.
- Canonical vaults never take on more than 3x their NAV in position, or 0.75x per fill, on top of Percolator's margin rules.
- `RetireMarket` only succeeds with no shares outstanding, no queued requests, a flat LP portfolio and at least three epochs since listing; Percolator additionally refuses unless the asset is empty. The vault returns to pending listing and can be listed again, into the freed slot.
- The opener's fee share is exactly 10% of each harvest, is held in the buffer as a reserved liability (so it never counts toward NAV), and only the opener can claim it. A closed or frozen opener token account cannot block harvests, because nothing is paid to it during a harvest.

## Known limits

- **Unaudited.** Neither this program nor Percolator has been audited.
- **Percolator trust.** The vault inherits Percolator's correctness. Its upstream suite still has open findings; see the `FINDINGS.md` of the integration branch.
- **Market authority.** Whoever holds `marketauth` can shut down the vault's asset (with Percolator's exit window) or resolve the market; the vault then winds down through `SettleResolved`. On devnet it is the governor PDA, so nobody can, and permissionless stale resolution is off. On a market we do not govern, this power stays with its authority.
- **Directional risk.** The vault is the counterparty to all traders. If traders win overall, depositors lose. Fees are the compensation.
- **Front-running (open).** Every fill happens immediately, at the asset's mark, which follows Pyth. A trader who sees the real price first can trade against the vault at a stale mark. Percolator's per-slot price cap and the 300 s staleness bound do not stop this; in a fast move the capped mark lags further. The fix, two-step fills through a permissionless router that executes each queued request at the first Pyth price published after it, is designed but not built (see the docs page, "Roadmap").
- **Liveness needs keepers.** Rolls, harvests, cranks, settling out-of-date positions, matcher renewal and retiring idle markets are permissionless, but someone has to send them. The provided keeper does.
- **Percolator fixes.** Devnet runs our Percolator integration branch (program 27d758ed), which includes fixes not yet merged upstream, among them permissionless listing while traders hold open PnL (percolator-prog #447).
- **Attach mode.** A vault can also provide liquidity on an asset listed by someone else. Its safety then depends on that asset's oracle authority, and the frontend should show this.
- **Unwind price.** `Unwind` closes at Percolator's effective price within its unilateral close capacity. It may take several calls in a stressed market.
