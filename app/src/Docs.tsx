import M from "./devnet.json";
import { CANON_EPOCH_LEN_SLOTS, ROUTER, ROUTER_DELAY_SECS, ROUTER_GRACE_SECS, slotSeconds } from "./chain";

const ex = (k: string) => `https://explorer.solana.com/address/${k}?cluster=devnet`;
const A = ({ k }: { k: string }) => <a className="mono" href={ex(k)} target="_blank" rel="noreferrer">{k}</a>;

const TOC = [
  ["overview", "Overview"],
  ["percolator", "Percolator in one minute"],
  ["problem", "The problem"],
  ["frontrun", "Front-running, solved"],
  ["design", "Design"],
  ["lifecycle", "Market lifecycle"],
  ["rules", "Market rules"],
  ["pricing", "Share pricing"],
  ["fees", "Fees and yield"],
  ["risks", "Risks"],
  ["fills", "How trades fill"],
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
      <g className="d-box"><rect x="20" y="40" width="170" height="64" rx="10" /><text x="105" y="68">Trader</text><text x="105" y="88" className="d-sub">wallet + router account</text></g>
      <g className="d-box strong"><rect x="330" y="30" width="240" height="84" rx="10" /><text x="450" y="62">Percolator program</text><text x="450" y="82" className="d-sub">risk engine: margin, PnL, funding,</text><text x="450" y="98" className="d-sub">liquidation, insurance</text></g>
      <g className="d-box"><rect x="710" y="40" width="170" height="64" rx="10" /><text x="795" y="68">Pyth</text><text x="795" y="88" className="d-sub">price feed account</text></g>
      <g className="d-box accent"><rect x="330" y="200" width="240" height="84" rx="10" /><text x="450" y="232">Vault program</text><text x="450" y="252" className="d-sub">matcher + pool + market owner</text><text x="450" y="268" className="d-sub">no admin instruction</text></g>
      <g className="d-box"><rect x="20" y="210" width="170" height="64" rx="10" /><text x="105" y="238">Liquidity providers</text><text x="105" y="258" className="d-sub">deposit, share 90% of fees</text></g>
      <g className="d-box"><rect x="710" y="210" width="170" height="64" rx="10" /><text x="795" y="238">Market opener</text><text x="795" y="258" className="d-sub">earns 10% of fees</text></g>
      <g className="d-box"><rect x="330" y="340" width="240" height="64" rx="10" /><text x="450" y="368">Keeper (anyone)</text><text x="450" y="388" className="d-sub">marks, fills, rolls, harvests</text></g>
      <path className="d-line" d="M190,72 L330,72" markerEnd="url(#arr)" /><text x="260" y="62" className="d-label">request → router</text>
      <path className="d-line" d="M710,72 L570,72" markerEnd="url(#arr)" /><text x="640" y="62" className="d-label">verified price</text>
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
          its trading fees to the people who deposit, and to whoever opened it. Every trade fills at the first Pyth price
          published {ROUTER_DELAY_SECS} seconds after it was requested, so nobody can trade against a price they already know.
          Nobody holds a key that can pause, change or drain any of it.
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
          <li><b>Filling at an oracle price invites front-running.</b> A market maker that fills immediately at the current oracle price can be traded against by anyone who sees the real price first.</li>
        </ol>

        <h2 id="frontrun">Front-running, solved</h2>
        <p><b>Who sees prices before Pyth?</b> Almost every professional trader:</p>
        <ul>
          <li><b>Traders watching the exchanges.</b> Pyth aggregates prices from exchanges and trading firms (Binance, OKX, Coinbase, Jane Street, Jump and others). Anyone connected to those venues sees a move before Pyth does.</li>
          <li><b>Pyth's publishers,</b> who know their own quotes before Pyth aggregates them.</li>
          <li><b>Anyone reading Pyth off-chain.</b> A new Pyth price exists on Pythnet and in Hermes, Pyth's price service, before anyone posts it on Solana, and posting is pull-based: the on-chain account is only as fresh as the last person who paid to update it. We measured Pyth's shared devnet account at 225 seconds old.</li>
          <li><b>Validators and searchers,</b> who see pending transactions, price updates included, and can put their own trades first.</li>
        </ul>
        <p>
          Before the router, the vault filled every trade immediately at Percolator's mark, which followed Pyth, so any of them could
          buy from it below the real price and sell to it above, and depositors paid. Now every trade is a request that fills at a
          price nobody could know when it was made (below). That is the problem this solves: <b>no one can trade against the vault
          at a price they already know, and no one can choose the price a trade fills at.</b>
        </p>

        <h2 id="design">Design</h2>
        <Diagram />
        <p>Two programs. The vault program does four jobs at once:</p>
        <ul>
          <li><b>Market owner.</b> The vault's own address signs the listing, so the vault becomes the asset's admin, insurance operator, backing authority and oracle authority. The asset is listed in Percolator's authority-mark mode at a fresh, verified Pyth price, and the vault moves its mark only when the router asks, to a verified Pyth price. The program has no other instruction that uses those powers.</li>
          <li><b>Matcher.</b> The vault account doubles as the matcher context. When the router executes a trade against the vault's LP portfolio, Percolator calls the vault program, which returns a fill size within its caps. It only answers calls signed by Percolator's matcher delegate for this vault's own portfolio, and only for the one fill the router armed in that slot.</li>
          <li><b>Pool.</b> Depositors hold vault shares (an SPL token). Deposits and withdrawals queue during an epoch and settle together at one price.</li>
          <li><b>One market per asset.</b> The vault address is derived from <code>(market, Pyth feed)</code>. A second vault for the same feed cannot exist, so all liquidity for an asset is in one pool and every fill sees all of it.</li>
        </ul>
        <p>
          The <b>router program</b> (<code>{ROUTER.toBase58().slice(0, 4)}…</code>) is the only way to trade: it queues requests,
          moves each vault's mark to verified Pyth prices, and executes fills. Each trader's Percolator account is owned by a
          program address derived from their wallet; only the router can trade it, and withdrawals can only go back to that wallet.
        </p>

        <h2 id="lifecycle">Market lifecycle</h2>
        <ol className="steps-doc">
          <li><b>Open.</b> <code>InitVault</code> creates the vault, its share mint, buffer and escrow, and its Percolator LP portfolio, and approves the vault program as that portfolio's matcher. <code>ListAsset</code> then activates the asset and configures its Pyth feed. Anyone can call either; if someone creates a vault and never lists it, anyone else can finish.</li>
          <li><b>Deposit.</b> <code>RequestDeposit</code> moves collateral into the vault's buffer and records a ticket for the current epoch.</li>
          <li><b>Roll.</b> When an epoch ends and the LP portfolio is flat, <code>RollEpoch</code> (anyone) withdraws all capital from Percolator, prices every queued withdrawal and deposit, burns and mints shares, redeploys everything that is not owed, and renews the matcher approval.</li>
          <li><b>Claim.</b> Each depositor's <code>Claim</code> collects their shares or collateral from the settled epoch, pro rata from the epoch record.</li>
          <li><b>Trade.</b> Takers queue requests with the router; executors fill them at the first Pyth price at or after each request's target, with the vault as counterparty (see How trades fill).</li>
          <li><b>Harvest.</b> <code>HarvestFees</code> (anyone) moves insurance above the floor into the buffer; 10% is set aside for the opener.</li>
          <li><b>Stay live.</b> After an epoch ends with requests waiting, the vault only takes position-reducing fills. If a taker holds a position so the vault can't go flat, <code>Unwind</code> (anyone, one epoch later) makes the vault close its own position through Percolator's unilateral <code>RebalanceReduce</code>.</li>
          <li><b>Retire.</b> Once a market has had no liquidity providers for three epochs and holds no position, <code>RetireMarket</code> (anyone) pays its leftover insurance to the opener and has Percolator retire the asset. The next market opened reuses the slot, and the same feed can be opened again later.</li>
          <li><b>Wind down.</b> If the market is resolved, <code>SettleResolved</code> closes the portfolio through Percolator's resolved path; queued deposits are refunded, and shares redeem pro rata with <code>RedeemTerminal</code>.</li>
        </ol>

        <h2 id="rules">Market rules</h2>
        <p>Every market opened here runs on the same fixed template, so nobody can open a market set up to fail. The program ignores whatever parameters the opener sends.</p>
        <table className="doc-table">
          <tbody>
            <tr><td>Vault position limit</td><td>3× the vault's NAV in notional, at the current price</td></tr>
            <tr><td>Largest single fill</td><td>0.75× the vault's NAV</td></tr>
            <tr><td>Epoch</td><td>{CANON_EPOCH_LEN_SLOTS.toLocaleString()} slots (about {Math.round(CANON_EPOCH_LEN_SLOTS * slotSeconds() / 60)} minutes at the current slot time) on devnet</td></tr>
            <tr><td>Insurance floor</td><td>100 USDC kept in insurance for traders; only fees above it are harvested</td></tr>
            <tr><td>Price</td><td>Verified Pyth prices only, moved forward by the router; listing needs a price at most 300 seconds old</td></tr>
            <tr><td>Execution</td><td>The first Pyth price published at or after {ROUTER_DELAY_SECS} seconds after the request landed</td></tr>
            <tr><td>Leverage and fee</td><td>10× (10% initial margin) and a 0.05% base fee per side, set by the market</td></tr>
            <tr><td>Opener's share</td><td>10% of harvested fees, forever, with no other powers</td></tr>
            <tr><td>Matcher approval</td><td>216,000 slots (about {Math.round(216_000 * slotSeconds() / 3600)} hours), renewed at every roll; anyone can renew it with <code>RefreshMatcher</code></td></tr>
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
          <li><b>Chain clock.</b> A request's target comes from the chain clock (or the mark time, if later). The {ROUTER_DELAY_SECS}-second delay assumes the clock does not run more than a few seconds behind real time; Solana's clock normally tracks it within a second or two.</li>
          <li><b>A request that cannot fill.</b> If a request cannot fill (for example after a price move larger than its 10% margin buffer), the mark waits at its price until it expires, at most {ROUTER_GRACE_SECS} seconds, before moving on. The requester loses the bond; liquidations on that market wait that long.</li>
          <li><b>Executors.</b> Moving marks and filling needs someone to post Pyth updates from Hermes (which needs an API key) and send the transactions. Anyone can; if nobody does, requests expire and nothing trades.</li>
          <li><b>Market authority.</b> On devnet, Percolator's market-level authority belongs to the vault program's governor address, which the program only uses to retire idle markets. Nobody can shut an asset down or resolve the market, and permissionless stale resolution is off. On another market, whoever holds that authority can shut assets down or resolve the market; the vault then winds down and pays out.</li>
          <li><b>Smart contracts.</b> Unaudited code. The program is upgradeable on devnet; a mainnet deployment must burn that authority.</li>
          <li><b>Keepers.</b> Everything is permissionless, but someone has to send the transactions. If nobody does, epochs don't roll and prices don't update.</li>
          <li><b>Percolator fixes.</b> Devnet runs our Percolator integration branch, which includes fixes not yet merged upstream, among them the one that makes permissionless listing work while traders hold open positions (<a href="https://github.com/aeyakovenko/percolator-prog/pull/447" target="_blank" rel="noreferrer">percolator-prog #447</a>).</li>
        </ul>

        <h2 id="fills">How trades fill</h2>
        <ol>
          <li><b>Request.</b> The trader queues a trade (a size; the fill is at the mark). It lands on chain and gets a target time: the later of the chain clock and the vault's current mark time, plus {ROUTER_DELAY_SECS} seconds. The trader's margin is locked from now until it fills or expires, and the request is only accepted with enough margin for a further 10% price move. It takes a small bond.</li>
          <li><b>The price.</b> It fills at exactly one price: the first Pyth price published at or after the target. Every Pyth update is signed with its publish time and the previous one's, so the router accepts only the update with <code>previous publish time &lt; target ≤ publish time</code>. Nobody could have known it when the request landed, and nobody can substitute another.</li>
          <li><b>The mark.</b> Executors move the vault's mark with verified Pyth updates, always forward in time. The router never moves it past a pending request's target: once that target has passed, the only update it accepts is the first one at or after it. Percolator's price then walks to the mark within its per-slot cap.</li>
          <li><b>Fill.</b> Once the mark is the request's price and Percolator's price has reached it, anyone can execute the fill, the trader included, and earns the bond. Who executes it, and when, does not change the price. The vault's matcher only accepts the fill the router armed for that slot and size, so there is no other way to trade against the vault.</li>
          <li><b>No backing out.</b> A request cannot be cancelled. If nobody fills it within {ROUTER_GRACE_SECS} seconds of its target it expires, nothing is traded, and its bond is forfeited.</li>
        </ol>
        <p>The router has no admin: no pause, no allow or block list, and its upgrade authority is to be burned before mainnet.</p>

        <h2 id="security">Security model</h2>
        <table className="doc-table">
          <thead><tr><th>Party</th><th>Can</th><th>Cannot</th></tr></thead>
          <tbody>
            <tr><td>Opener</td><td>Pick the feed; claim 10% of harvested fees</td><td>Choose any parameter; open a second market for a feed; move funds; pause</td></tr>
            <tr><td>Depositors</td><td>Queue deposits and withdrawals, claim, redeem after wind-down</td><td>Affect other depositors' prices</td></tr>
            <tr><td>Anyone</td><td>Roll, harvest, convert, renew, unwind, settle, sweep, retire an idle market</td><td>Choose amounts or destinations: value only moves into the vault's own accounts, or to the opener when a market is retired</td></tr>
            <tr><td>Traders</td><td>Queue trades through the router; move margin in, and out to their own wallet</td><td>Trade at a price they know, cancel a queued trade, withdraw while one is queued, trade against the vault any other way, or lock withdrawals</td></tr>
            <tr><td>Executors (anyone)</td><td>Move a mark to a newer verified Pyth price; fill a request at its price and earn its bond; expire late requests</td><td>Choose a fill price, move the mark past a pending request's price or backwards, or fill early</td></tr>
            <tr><td>Governor (program address)</td><td>Retire an idle, empty market through <code>RetireMarket</code></td><td>Anything else: the program has no other instruction that uses Percolator's market authority</td></tr>
          </tbody>
        </table>
        <p>The program is tested against the production Percolator binary in LiteSVM, with 51 tests:</p>
        <ul>
          <li><b>Front-running</b>: a trader who knows the price in advance ends flat; the fill is exactly the first Pyth price at or after the target; no later or skipped update can move the mark; nobody can push a newer price over a pending request; the executor's identity and timing do not change the fill; direct trades, forged or unverified Pyth accounts and non-router mark moves are refused.</li>
          <li><b>Requests</b>: no cancelling, no withdrawing while queued, expiry only after the grace period with the bond forfeited, stressed margin check, withdrawals only to the owner's wallet.</li>
          <li><b>Property tests</b>: withdrawals never exceed NAV, deposit-then-withdraw never profits, incumbents are never diluted, fills never break the caps.</li>
          <li><b>Layout tests</b>: every account offset and every Percolator instruction encoding is checked against Percolator's own types and decoder.</li>
          <li><b>Flows</b>: open, deposit, trade, roll, withdraw; fee harvest and the opener's share; gains and losses reaching depositors exactly; maintenance fees; market resolution with everyone paid out; one vault per feed; caps scaling with NAV; retiring an idle market and reopening its feed in the freed slot.</li>
          <li><b>Attacks</b>: forged matcher calls, a foreign LP routing fills through the vault, inventory caps, rolling while not flat, reduce-only enforcement, matcher expiry and renewal, early, doubled, stolen and forged claims, fake buffers, donations, the inflation attack, bad parameters, double listing, sweeps, and a trader holding a position to lock withdrawals.</li>
        </ul>

        <h2 id="instructions">Instruction reference</h2>
        <table className="doc-table">
          <thead><tr><th>Tag</th><th>Instruction</th><th>Who</th><th>Effect</th></tr></thead>
          <tbody>
            <tr><td>0</td><td>matcher call</td><td>Percolator only</td><td>Sizes the fill the router armed, during its <code>TradeCpi</code></td></tr>
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
            <tr><td>31</td><td>RetireMarket</td><td>anyone</td><td>Frees the slot of an idle market (no shares, no requests, no position, listed 3+ epochs ago); leftover insurance goes to the opener</td></tr>
            <tr><td>30</td><td>AcceptGovernance</td><td>market authority</td><td>One-time: hands Percolator's market authority to the program's governor address</td></tr>
            <tr><td>32</td><td>PushMark</td><td>router only</td><td>Moves the asset's mark (the router passes only verified Pyth prices)</td></tr>
            <tr><td>33</td><td>ArmFill</td><td>router only</td><td>Arms the one fill the matcher accepts in this slot</td></tr>
          </tbody>
        </table>
        <p>Router program:</p>
        <table className="doc-table">
          <thead><tr><th>Tag</th><th>Instruction</th><th>Who</th><th>Effect</th></tr></thead>
          <tbody>
            <tr><td>0</td><td>OpenAccount</td><td>trader</td><td>Creates the trader's router account, Percolator portfolio and collateral account</td></tr>
            <tr><td>1 / 2</td><td>Deposit / Withdraw</td><td>trader</td><td>Margin in; margin out to the trader's own wallet (not while a request is queued)</td></tr>
            <tr><td>3</td><td>OpenBook</td><td>anyone</td><td>Creates a vault's queue of requests</td></tr>
            <tr><td>4</td><td>Request</td><td>trader</td><td>Queues a trade and fixes its target time</td></tr>
            <tr><td>5</td><td>Advance</td><td>anyone</td><td>Moves a vault's mark to a newer verified Pyth price, never past a pending target</td></tr>
            <tr><td>6</td><td>Fill</td><td>anyone</td><td>Fills a request at its price; pays the executor the bond</td></tr>
            <tr><td>7</td><td>Expire</td><td>anyone</td><td>Removes a request unfilled {ROUTER_GRACE_SECS} s after its target; the bond is forfeited</td></tr>
          </tbody>
        </table>

        <h2 id="keeper">Keepers</h2>
        <p>
          Nothing needs a trusted operator, but someone has to send maintenance transactions. The reference keeper
          (<code>cargo run --example devnet --features devnet -- keeper</code>) discovers every vault of the program and, for each:
          keeps its asset's price accrual current (each crank advances it by at most 10 slots; idle markets are kept within 40
          slots so their first trade can catch up), settles positions left out of date by a price move, harvests fees above the
          floor, converts released profit, rolls the epoch when it ends, calls <code>Unwind</code> if an epoch is overdue by a
          full epoch, and retires idle markets. Anyone can run another one.
        </p>
        <p>
          The executor (<code>app/scripts/executor.ts</code>) runs the router: it posts Pyth updates from Hermes and moves each
          vault's mark to them (to the first update at or after the earliest pending target once that exists, otherwise to the
          latest one every couple of seconds), fills requests whose price the mark has reached, and expires late ones. It needs
          a Hermes API key. Anyone can run one; the program checks everything, and each fill pays its executor the request's bond.
        </p>

        <h2 id="integrate">Integrating</h2>
        <p>
          Trading goes through the router: <code>OpenAccount</code> once, <code>Deposit</code> margin, then <code>Request</code> a
          size against a vault. The request fills a few seconds later, at the first Pyth price published at or after its target;
          nothing needs to be signed after the request. Calling Percolator's <code>TradeCpi</code> against a vault directly is
          refused by its matcher.
        </p>
        <p>
          The addresses and layouts are exported as <code>app/src/layout.json</code>. The TypeScript builders in
          <code>app/src/chain.ts</code> and the Rust builders in <code>src/client.rs</code> and <code>router/src/client.rs</code>
          cover every instruction.
        </p>

        <h2 id="addresses">Devnet addresses</h2>
        <table className="doc-table">
          <tbody>
            <tr><td>Vault program</td><td><A k={M.vault_program} /></td></tr>
            <tr><td>Router program</td><td><A k={ROUTER.toBase58()} /></td></tr>
            <tr><td>Percolator program</td><td><A k={M.percolator_program} /></td></tr>
            <tr><td>Market</td><td><A k={M.market} /></td></tr>
            <tr><td>Test USDC</td><td><A k={M.collateral_mint} /></td></tr>
          </tbody>
        </table>
        <p className="muted small">The Percolator program was deployed from the exact build the vault is tested against (engine 6de466b, program 27d758ed on the integration branch). The market's authority is the vault program's governor address.</p>

        <h2 id="mainnet">Path to mainnet</h2>
        <ol>
          <li>Build against the final, audited Percolator program ID (a compile-time constant; the devnet build already swaps it).</li>
          <li>Create a market with permissionless listing enabled and hand its market authority to the governor with <code>AcceptGovernance</code>, or list into an existing market (whose authority then keeps its powers).</li>
          <li>Run executors with Hermes access, and burn the router's upgrade authority along with the vault's.</li>
          <li>Audit the vault program, then burn its upgrade authority (or put it behind a public timelock).</li>
          <li>Set mainnet template values (for example one-hour epochs) and a real collateral with its freeze authority revoked.</li>
          <li>Run redundant keepers and a Pyth price pusher.</li>
        </ol>

        <h2 id="faq">FAQ</h2>
        <dl className="faq">
          <dt>Why does a trade fill at the mark, not at a quoted price?</dt>
          <dd>That is how Percolator v16 works: the matcher's price only sizes fees. The vault earns from fees and from traders' losses, not from a spread.</dd>
          <dt>Why does my trade take a few seconds?</dt>
          <dd>It fills at the first Pyth price published {ROUTER_DELAY_SECS} seconds after your request landed. That wait is what makes the price one nobody, including you, could know in advance, so nobody can trade against the vault, or against you, ahead of it.</dd>
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
