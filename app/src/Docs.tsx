import M from "./devnet.json";

const ex = (k: string) => `https://explorer.solana.com/address/${k}?cluster=devnet`;
const A = ({ k }: { k: string }) => <a className="mono" href={ex(k)} target="_blank" rel="noreferrer">{k}</a>;

const TOC = [
  ["overview", "Overview"],
  ["percolator", "Percolator in one minute"],
  ["problem", "The problem"],
  ["design", "Design"],
  ["lifecycle", "Market lifecycle"],
  ["rules", "Market rules"],
  ["pricing", "Share pricing"],
  ["fees", "Fees and yield"],
  ["risks", "Risks"],
  ["security", "Security model"],
  ["instructions", "Instruction reference"],
  ["keeper", "Keepers"],
  ["integrate", "Integrating"],
  ["addresses", "Devnet addresses"],
  ["mainnet", "Path to mainnet"],
  ["faq", "FAQ"],
];

function Diagram() {
  // Arrows show value and control flow; boxes are programs, accounts and people.
  return (
    <svg className="diagram" viewBox="0 0 900 430" role="img" aria-label="How a trade, fees and deposits flow between traders, Percolator and the vault">
      <defs>
        <marker id="arr" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
          <path d="M0,0 L10,5 L0,10 z" className="d-head" />
        </marker>
      </defs>
      <g className="d-box"><rect x="20" y="40" width="170" height="64" rx="10" /><text x="105" y="68">Trader</text><text x="105" y="88" className="d-sub">wallet + Percolator account</text></g>
      <g className="d-box strong"><rect x="330" y="30" width="240" height="84" rx="10" /><text x="450" y="62">Percolator program</text><text x="450" y="82" className="d-sub">risk engine: margin, PnL, funding,</text><text x="450" y="98" className="d-sub">liquidation, insurance</text></g>
      <g className="d-box"><rect x="710" y="40" width="170" height="64" rx="10" /><text x="795" y="68">Pyth</text><text x="795" y="88" className="d-sub">price feed account</text></g>
      <g className="d-box accent"><rect x="330" y="200" width="240" height="84" rx="10" /><text x="450" y="232">Vault program</text><text x="450" y="252" className="d-sub">matcher + pool + market owner</text><text x="450" y="268" className="d-sub">no admin instruction</text></g>
      <g className="d-box"><rect x="20" y="210" width="170" height="64" rx="10" /><text x="105" y="238">Liquidity providers</text><text x="105" y="258" className="d-sub">deposit, share 90% of fees</text></g>
      <g className="d-box"><rect x="710" y="210" width="170" height="64" rx="10" /><text x="795" y="238">Market opener</text><text x="795" y="258" className="d-sub">earns 10% of fees</text></g>
      <g className="d-box"><rect x="330" y="340" width="240" height="64" rx="10" /><text x="450" y="368">Keeper (anyone)</text><text x="450" y="388" className="d-sub">cranks, rolls, harvests</text></g>
      <path className="d-line" d="M190,72 L330,72" markerEnd="url(#arr)" /><text x="260" y="62" className="d-label">TradeCpi</text>
      <path className="d-line" d="M710,72 L570,72" markerEnd="url(#arr)" /><text x="640" y="62" className="d-label">price</text>
      <path className="d-line" d="M430,114 L430,200" markerEnd="url(#arr)" /><text x="424" y="160" className="d-label end">asks for a fill</text>
      <path className="d-line" d="M470,200 L470,114" markerEnd="url(#arr)" /><text x="478" y="160" className="d-label start">size, at mark</text>
      <path className="d-line" d="M190,242 L330,242" markerEnd="url(#arr)" /><text x="260" y="232" className="d-label">deposits</text>
      <path className="d-line" d="M570,242 L710,242" markerEnd="url(#arr)" /><text x="640" y="232" className="d-label">10% of fees</text>
      <path className="d-line dash" d="M450,340 L450,284" markerEnd="url(#arr)" />
    </svg>
  );
}

export default function Docs() {
  return (
    <main className="docs">
      <aside className="toc">
        <div className="toc-title">Documentation</div>
        {TOC.map(([id, t]) => <a key={id} href={`#/docs`} onClick={e => { e.preventDefault(); document.getElementById(id)?.scrollIntoView({ behavior: "smooth" }); }}>{t}</a>)}
      </aside>
      <article className="doc">
        <h1 id="overview">Percolator Vaults</h1>
        <p className="lead">
          Keyless perpetual markets with liquidity built in, on Anatoly Yakovenko's Percolator risk engine. Anyone can open the
          market for a Pyth price feed. A vault program then owns that market, makes markets on it from one shared pool, and pays
          its trading fees to the people who deposit, and to whoever opened it. Nobody holds a key that can pause, change or
          drain it.
        </p>
        <div className="callout">
          <b>Status.</b> Live on Solana devnet. Unaudited: neither this program nor Percolator has been audited. Use test funds only.
          Source: <a href="https://github.com/Commoneffort/percolator-vaults" target="_blank" rel="noreferrer">github.com/Commoneffort/percolator-vaults</a>.
        </div>

        <h2 id="percolator">Percolator in one minute</h2>
        <p>
          <a href="https://github.com/aeyakovenko/percolator" target="_blank" rel="noreferrer">Percolator</a> is a perpetual-futures
          risk engine and Solana program. A <i>market</i> account holds many <i>assets</i> (asset 0 is the collateral unit; the rest
          are perps priced in it). Each trader has a <i>portfolio</i> account with collateral and up to a few cross-margined positions.
        </p>
        <ul>
          <li><b>Trades settle at the market's mark price.</b> A trade names a counterparty portfolio (an LP) whose <i>matcher</i> program Percolator calls to decide the fill size. The price a matcher quotes only sizes fees; profit and loss come from the mark moving.</li>
          <li><b>Fees go to insurance.</b> Every trade pays a base fee on both sides into the traded asset's insurance budget, which absorbs bad debt. That asset's <i>insurance operator</i> can withdraw budget above what is needed, when the market is healthy.</li>
          <li><b>Listing is permissionless.</b> Anyone can activate a new asset by paying a fee. The activating signer becomes the asset's admin and names its insurance, backing and oracle authorities.</li>
          <li><b>Safety is engineered in.</b> Prices can only move a bounded amount per slot, positive PnL is only usable when backed, bad debt stays inside the asset that caused it, and anyone can crank the market forward in bounded steps.</li>
        </ul>

        <h2 id="problem">The problem</h2>
        <ol>
          <li><b>New markets are empty.</b> A freshly listed asset has no liquidity, so nobody can trade it. Percolator's own mainnet market has no market maker.</li>
          <li><b>Whoever lists a market holds its keys.</b> The lister becomes admin, insurance operator and oracle authority: they can shut the market down, rotate its keys or withdraw its insurance. Traders have to trust them.</li>
          <li><b>Liquidity fragments.</b> If every listing is a separate market, three SOL markets means three thin books.</li>
          <li><b>A spread earns nothing.</b> Because trades settle at the mark, a classic spread-quoting market maker has no edge. The income is in the fees, which belong to whoever operates the asset.</li>
        </ol>

        <h2 id="design">Design</h2>
        <Diagram />
        <p>The vault program does four jobs at once:</p>
        <ul>
          <li><b>Market owner.</b> The vault's own address signs the listing, so the vault becomes the asset's admin, insurance operator, backing authority and oracle authority. The program has no instruction that uses those powers, and the Pyth feed is configured once, from fixed parameters.</li>
          <li><b>Matcher.</b> The vault account doubles as the matcher context. When a taker trades against the vault's LP portfolio, Percolator calls the vault program, which returns a fill size within its caps. It only answers calls signed by Percolator's matcher delegate for this vault's own portfolio.</li>
          <li><b>Pool.</b> Depositors hold vault shares (an SPL token). Deposits and withdrawals queue during an epoch and settle together at one price.</li>
          <li><b>One market per asset.</b> The vault address is derived from <code>(market, Pyth feed)</code>. A second vault for the same feed cannot exist, so all liquidity for an asset is in one pool and every fill sees all of it.</li>
        </ul>

        <h2 id="lifecycle">Market lifecycle</h2>
        <ol className="steps-doc">
          <li><b>Open.</b> <code>InitVault</code> creates the vault, its share mint, buffer and escrow, and its Percolator LP portfolio, and approves the vault program as that portfolio's matcher. <code>ListAsset</code> then activates the asset and configures its Pyth feed. Anyone can call either; if someone creates a vault and never lists it, anyone else can finish.</li>
          <li><b>Deposit.</b> <code>RequestDeposit</code> moves collateral into the vault's buffer and records a ticket for the current epoch.</li>
          <li><b>Roll.</b> When an epoch ends and the LP portfolio is flat, <code>RollEpoch</code> (anyone) withdraws all capital from Percolator, prices every queued withdrawal and deposit, burns and mints shares, redeploys everything that is not owed, and renews the matcher approval.</li>
          <li><b>Claim.</b> Each depositor's <code>Claim</code> collects their shares or collateral from the settled epoch, pro rata from the epoch record.</li>
          <li><b>Trade.</b> Takers trade through Percolator's <code>TradeCpi</code> with the vault as counterparty.</li>
          <li><b>Harvest.</b> <code>HarvestFees</code> (anyone) moves insurance above the floor into the buffer; 10% is set aside for the opener.</li>
          <li><b>Stay live.</b> After an epoch ends with requests waiting, the vault only takes position-reducing fills. If a taker holds a position so the vault can't go flat, <code>Unwind</code> (anyone, one epoch later) makes the vault close its own position through Percolator's unilateral <code>RebalanceReduce</code>.</li>
          <li><b>Wind down.</b> If the market is resolved, <code>SettleResolved</code> closes the portfolio through Percolator's resolved path; queued deposits are refunded, and shares redeem pro rata with <code>RedeemTerminal</code>.</li>
        </ol>

        <h2 id="rules">Market rules</h2>
        <p>Every market opened here runs on the same fixed template, so nobody can open a market set up to fail. The program ignores whatever parameters the opener sends.</p>
        <table className="doc-table">
          <tbody>
            <tr><td>Vault position limit</td><td>3× the vault's NAV in notional, at the current price</td></tr>
            <tr><td>Largest single fill</td><td>0.75× the vault's NAV</td></tr>
            <tr><td>Epoch</td><td>1,500 slots (about 10 minutes) on devnet</td></tr>
            <tr><td>Insurance floor</td><td>100 USDC kept in insurance for traders; only fees above it are harvested</td></tr>
            <tr><td>Price</td><td>Pyth, at most 300 seconds old; Percolator falls back to a smoothed mark when stale</td></tr>
            <tr><td>Leverage and fee</td><td>10× (10% initial margin) and a 0.05% base fee per side, set by the market</td></tr>
            <tr><td>Opener's share</td><td>10% of harvested fees, forever, with no other powers</td></tr>
            <tr><td>Matcher approval</td><td>renewed for about a day at every roll; anyone can renew it with <code>RefreshMatcher</code></td></tr>
          </tbody>
        </table>
        <p>Because limits are multiples of NAV, an empty vault quotes nothing and every deposit makes the market deeper. Percolator's own margin checks apply on top.</p>

        <h2 id="pricing">Share pricing</h2>
        <p>Shares use ERC-4626-style virtual offsets so the first-depositor inflation attack loses money. At a roll, with <code>S</code> the share supply:</p>
        <pre>{`NAV_low  = buffer − owed − queued deposits           (collateral actually held)
NAV_high = NAV_low + booked-but-unreleased profit + 90% × unharvested fees

withdrawal:  assets = shares × (NAV_low + 1) / (S + 1000)          (rounded down, ≤ NAV_low)
deposit:     shares = assets × (S' + 1000) / (NAV_high' + 1)       (rounded down)`}</pre>
        <ul>
          <li>Withdrawals are priced low and deposits high, so neither side can take value from the other. Every rounding step goes against the party acting.</li>
          <li>Pricing only happens when the LP portfolio is proven flat, so there is no mark-to-market to game.</li>
          <li>If the vault has lost everything while shares exist, that epoch's deposits are refunded instead of priced.</li>
        </ul>

        <h2 id="fees">Fees and yield</h2>
        <ul>
          <li>Every trade pays the market's base fee (0.05%) on both sides into the asset's insurance. On a trade against the vault, the vault pays its side too, and gets both back when fees are harvested.</li>
          <li>Harvested fees above the 100 USDC floor are split 90% to depositors (by raising the share price) and 10% to the opener (claimable with <code>ClaimOpenerFees</code>).</li>
          <li>The vault is the counterparty to traders, so its result is <b>fees + what traders lose − what traders win</b>. Fees are the compensation for that exposure.</li>
        </ul>

        <h2 id="risks">Risks</h2>
        <ul>
          <li><b>Directional.</b> If traders win overall, depositors lose. Caps bound the vault's exposure to 3× its NAV.</li>
          <li><b>Oracle latency.</b> Traders who see prices before Pyth can pick off the vault. Percolator's per-slot price cap and the staleness bound limit this; they do not remove it.</li>
          <li><b>Market authority.</b> The Percolator market's own admin can shut an asset down (with an exit window) or resolve the market. The vault then winds down and pays out.</li>
          <li><b>Smart contracts.</b> Unaudited code. The program is upgradeable on devnet; a mainnet deployment must burn that authority.</li>
          <li><b>Keepers.</b> Everything is permissionless, but someone has to send the transactions. If nobody does, epochs don't roll and prices don't update.</li>
        </ul>

        <h2 id="security">Security model</h2>
        <table className="doc-table">
          <thead><tr><th>Party</th><th>Can</th><th>Cannot</th></tr></thead>
          <tbody>
            <tr><td>Opener</td><td>Pick the feed; claim 10% of harvested fees</td><td>Choose any parameter; open a second market for a feed; move funds; pause</td></tr>
            <tr><td>Depositors</td><td>Queue deposits and withdrawals, claim, redeem after wind-down</td><td>Affect other depositors' prices</td></tr>
            <tr><td>Anyone</td><td>Roll, harvest, convert, renew, unwind, settle, sweep</td><td>Choose amounts or destinations: value only moves into the vault's own accounts</td></tr>
            <tr><td>Takers</td><td>Trade within the caps</td><td>Call the matcher directly, route fills through another LP, or lock withdrawals</td></tr>
          </tbody>
        </table>
        <p>The program is tested against the production Percolator binary in LiteSVM, with 38 tests:</p>
        <ul>
          <li><b>Property tests</b>: withdrawals never exceed NAV, deposit-then-withdraw never profits, incumbents are never diluted, fills never break the caps.</li>
          <li><b>Layout tests</b>: every account offset and every Percolator instruction encoding is checked against Percolator's own types and decoder.</li>
          <li><b>Flows</b>: open, deposit, trade, roll, withdraw; fee harvest and the opener's share; gains and losses reaching depositors exactly; maintenance fees; market resolution with everyone paid out; one vault per feed; caps scaling with NAV.</li>
          <li><b>Attacks</b>: forged matcher calls, a foreign LP routing fills through the vault, inventory caps, rolling while not flat, reduce-only enforcement, matcher expiry and renewal, early, doubled, stolen and forged claims, fake buffers, donations, the inflation attack, bad parameters, double listing, sweeps, and a trader holding a position to lock withdrawals.</li>
        </ul>

        <h2 id="instructions">Instruction reference</h2>
        <table className="doc-table">
          <thead><tr><th>Tag</th><th>Instruction</th><th>Who</th><th>Effect</th></tr></thead>
          <tbody>
            <tr><td>0</td><td>matcher call</td><td>Percolator only</td><td>Sizes a fill during a taker's <code>TradeCpi</code></td></tr>
            <tr><td>16</td><td>InitVault</td><td>anyone</td><td>Creates the canonical vault for a feed and its accounts</td></tr>
            <tr><td>26</td><td>ListAsset</td><td>anyone</td><td>Lists the vault's asset and configures its Pyth feed</td></tr>
            <tr><td>17 / 18</td><td>RequestDeposit / RequestWithdraw</td><td>depositor</td><td>Queues a request for the current epoch</td></tr>
            <tr><td>19</td><td>RollEpoch</td><td>anyone</td><td>Settles the epoch while flat and redeploys capital</td></tr>
            <tr><td>20</td><td>Claim</td><td>depositor</td><td>Collects shares or collateral from a settled epoch</td></tr>
            <tr><td>27</td><td>HarvestFees</td><td>anyone</td><td>Moves insurance above the floor into the vault</td></tr>
            <tr><td>29</td><td>ClaimOpenerFees</td><td>opener</td><td>Collects the opener's accrued 10%</td></tr>
            <tr><td>22</td><td>ConvertPnl</td><td>anyone</td><td>Turns released profit into withdrawable capital</td></tr>
            <tr><td>21</td><td>RefreshMatcher</td><td>anyone</td><td>Renews the time-limited matcher approval</td></tr>
            <tr><td>28</td><td>Unwind</td><td>anyone</td><td>Liveness backstop: the vault closes its own position</td></tr>
            <tr><td>23 / 24</td><td>SettleResolved / RedeemTerminal</td><td>anyone / holder</td><td>Wind-down after market resolution</td></tr>
            <tr><td>25</td><td>Sweep</td><td>anyone</td><td>Moves stray vault-owned collateral into the buffer</td></tr>
          </tbody>
        </table>

        <h2 id="keeper">Keepers</h2>
        <p>
          Nothing needs a trusted operator, but someone has to send maintenance transactions. The reference keeper
          (<code>cargo run --example devnet --features devnet -- keeper</code>) discovers every vault of the program and, for each:
          cranks its asset while positions are open (each crank advances accrual by at most 10 slots), harvests fees above the
          floor, converts released profit, rolls the epoch when it ends, and calls <code>Unwind</code> if an epoch is overdue by a
          full epoch. Anyone can run another one.
        </p>

        <h2 id="integrate">Integrating</h2>
        <p>To trade against a vault, send Percolator's <code>TradeCpi</code> with the vault's LP portfolio as counterparty:</p>
        <pre>{`accounts: [trader (signer), market, trader portfolio, vault LP portfolio,
           vault program, vault account (matcher context), matcher delegate]
data:     tag 10 | taker portfolio id, position epoch | LP portfolio id, position epoch, sequence
          | asset index | asset market id | size (i128, +long) | max fee bps | limit price | backing fee cap`}</pre>
        <p>
          The vault's addresses are all in its account (layout exported as <code>app/src/layout.json</code>). The TypeScript
          builders in <code>app/src/chain.ts</code> and the Rust builders in <code>src/client.rs</code> cover every instruction.
          Each wallet's trading portfolio for an asset lives at <code>createWithSeed(wallet, "pv1-asset-&lt;index&gt;", Percolator)</code>.
        </p>

        <h2 id="addresses">Devnet addresses</h2>
        <table className="doc-table">
          <tbody>
            <tr><td>Vault program</td><td><A k={M.vault_program} /></td></tr>
            <tr><td>Percolator program</td><td><A k={M.percolator_program} /></td></tr>
            <tr><td>Market</td><td><A k={M.market} /></td></tr>
            <tr><td>Test USDC</td><td><A k={M.collateral_mint} /></td></tr>
          </tbody>
        </table>
        <p className="muted small">The Percolator program was deployed from the exact build the vault is tested against (engine 6de466b, program fac4cd66 on the integration branch).</p>

        <h2 id="mainnet">Path to mainnet</h2>
        <ol>
          <li>Build against the final, audited Percolator program ID (a compile-time constant; the devnet build already swaps it).</li>
          <li>Either create a market with permissionless listing enabled, or list into an existing one whose admin enables it.</li>
          <li>Audit the vault program, then burn its upgrade authority (or put it behind a public timelock).</li>
          <li>Set mainnet template values (for example one-hour epochs) and a real collateral with its freeze authority revoked.</li>
          <li>Run redundant keepers and a Pyth price pusher.</li>
        </ol>

        <h2 id="faq">FAQ</h2>
        <dl className="faq">
          <dt>Why does a trade fill at the mark, not at a quoted price?</dt>
          <dd>That is how Percolator v16 works: the matcher's price only sizes fees. The vault earns from fees and from traders' losses, not from a spread.</dd>
          <dt>Why do deposits wait for the epoch?</dt>
          <dd>Percolator only lets a portfolio withdraw when it holds no position. Settling everyone together at a flat point means nobody can buy or sell shares at a stale price.</dd>
          <dt>Can the same pair be listed twice?</dt>
          <dd>Not through this program: the vault address is derived from the feed, so the second attempt fails on-chain.</dd>
          <dt>What stops someone from blocking withdrawals by holding a position?</dt>
          <dd>Reduce-only quoting after the epoch, then <code>Unwind</code>, which lets anyone make the vault close its own position one epoch later.</dd>
          <dt>Who pays for maintenance?</dt>
          <dd>Whoever sends it. Each transaction costs a network fee only; a roll also pays a small rent for the epoch record.</dd>
          <dt>What is the "Devnet Burner" wallet?</dt>
          <dd>A key stored in your browser so you can try the app without installing a wallet. Devnet only; never use it for real funds.</dd>
        </dl>
      </article>
    </main>
  );
}
