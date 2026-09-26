# Security model

## Trust

| Party | Can | Cannot |
|---|---|---|
| Vault creator | Choose the parameters once, at creation | Change any parameter; move funds; pause; upgrade |
| Depositors | Queue deposits and withdrawals; claim; redeem after wind-down | Affect other depositors' prices |
| Anyone | Roll, harvest, convert, renew, unwind, settle, sweep | Choose amounts or destinations: each of these moves value only into the vault's own buffer or its own portfolio |
| Takers | Trade against the vault within its caps | Call the matcher directly; route fills through another LP; lock withdrawals |
| Percolator market authority (`marketauth`) | Force-shut down any asset (with an exit window); resolve the market | Take the vault's insurance or collateral |
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

## Known limits

- **Unaudited.** Neither this program nor Percolator has been audited.
- **Percolator trust.** The vault inherits Percolator's correctness. Its upstream suite still has open findings; see the `FINDINGS.md` of the integration branch.
- **Market authority.** `marketauth` can shut down the vault's asset (with Percolator's exit window) or resolve the market. The vault then winds down through `SettleResolved`.
- **Directional risk.** The vault is the counterparty to all traders. If traders win overall, depositors lose. Fees are the compensation.
- **Oracle latency.** Informed traders can pick off the vault when Pyth lags the real market. Percolator's per-slot price cap and the staleness bound limit this, but do not remove it.
- **Liveness needs keepers.** Rolls, harvests, cranks and matcher renewal are permissionless, but someone has to send them. The provided keeper does.
- **Attach mode.** A vault can also provide liquidity on an asset listed by someone else. Its safety then depends on that asset's oracle authority, and the frontend should show this.
- **Unwind price.** `Unwind` closes at Percolator's effective price within its unilateral close capacity. It may take several calls in a stressed market.
