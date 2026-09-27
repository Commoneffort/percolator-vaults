# Demo script (about 3 minutes)

Recorded against the live devnet app with a Devnet Burner wallet. The narration below is exactly what the generated video speaks (`demo/narration.json`, voiced by `demo/tts.py`); the screen actions are what `demo/record.py` records, and `demo/assemble.py` joins them into `docs/demo.mp4`.

| # | Screen | Narration |
|---|---|---|
| 1 | Title card | Percolator is Anatoly Yakovenko's perpetual futures engine for Solana. It can list any market without permission. But a new market starts empty, and whoever lists it holds its keys. |
| 2 | Markets page, slow scroll | Percolator Vaults fixes that. Anyone can open the market for a Pyth price feed. A vault program lists it, owns every key, and becomes its liquidity, from one shared pool per asset. Nobody can pause, change, or drain it. |
| 3 | Connect the burner wallet, get test USDC, open a market | Let's open a market. I connect a devnet burner wallet and grab test USDC. Opening takes two transactions: create the vault, then list the market with its Pyth feed. I choose nothing else. Every market runs on the same rules, and as the opener I earn ten percent of its fees, forever, with no other powers. |
| 4 | Bitcoin market: depth, open an account, add margin, go long | On the Bitcoin market, the vault is the counterparty. It sizes every fill from its own value, up to three times its net asset value, so every deposit deepens the market. I open a trading account, add margin, and go long. The trade settles at Percolator's mark price and shows up in the activity feed. |
| 5 | Provide liquidity tab, deposit | Liquidity providers all deposit into that same pool. Deposits and withdrawals settle together at one price at the end of each epoch, about six minutes on devnet, so nobody trades against a stale share price. Depositors share ninety percent of the market's fees. |
| 6 | Leaderboard | Every trader is ranked from on-chain data. Wallets we used to seed demo activity are labelled as seed. |
| 7 | Docs: diagram, rules, security | Under the hood, the vault's address is derived from the price feed, so an asset can never have two pools. The vault program is the matcher that Percolator calls on every trade. If a trader holds a position to block withdrawals, anyone can make the vault close its own position. Thirty-nine tests run it against Percolator's production binary, including attack scenarios. |
| 8 | Closing card with links | Percolator Vaults. Keyless perp markets, with liquidity built in. Live on Solana devnet today, and built to go to mainnet with Percolator. |

## Recording a live walkthrough yourself

1. Open https://percolator-vaults.vercel.app, click **Select Wallet → Devnet Burner** (or use Phantom on devnet).
2. **Get test USDC** (1,000 test USDC and 0.3 devnet SOL).
3. **Open a market** → pick a feed that isn't open yet → **Open** and approve.
4. Open **BTC-PERP** → **Create trading account** → **Add margin** 100 → size 0.01 → **Long** → **Close position**.
5. **Provide liquidity** → deposit 100 → watch the epoch counter; after it rolls, **Claim**.
6. **Leaderboard**, then **Docs**.
