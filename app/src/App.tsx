import { useCallback, useEffect, useMemo, useState } from "react";
import { useConnection, useWallet } from "@solana/wallet-adapter-react";
import { WalletMultiButton } from "@solana/wallet-adapter-react-ui";
import { ComputeBudgetProgram, PublicKey, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction } from "@solana/spl-token";
import * as C from "./chain";

const SLOT_SECONDS = 0.4;

const fmtUsd = (atoms: bigint, dp = 2) => (Number(atoms) / C.USDC).toLocaleString(undefined, { minimumFractionDigits: dp, maximumFractionDigits: dp });
const fmtPrice = (e6: bigint) => (Number(e6) / 1e6).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
const fmtSol = (q: bigint) => (Number(q) / Number(C.UNIT)).toLocaleString(undefined, { maximumFractionDigits: 4 });
const fmtShares = (s: bigint) => (Number(s) / 10 ** C.SHARE_DECIMALS).toLocaleString(undefined, { maximumFractionDigits: 4 });
const short = (k: PublicKey | string) => { const s = k.toString(); return `${s.slice(0, 4)}…${s.slice(-4)}`; };
const explorer = (what: "address" | "tx", id: string) => `https://explorer.solana.com/${what}/${id}?cluster=devnet`;

function useChain() {
  const { connection } = useConnection();
  const { publicKey } = useWallet();
  const [state, setState] = useState<C.State>();
  const [portfolioKey, setPortfolioKey] = useState<PublicKey>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    if (!publicKey) return setPortfolioKey(undefined);
    C.portfolioAddress(publicKey).then(setPortfolioKey);
  }, [publicKey]);

  const refresh = useCallback(async () => {
    try {
      setState(await C.fetchState(connection, publicKey ?? undefined, portfolioKey));
      setError(undefined);
    } catch (e: any) {
      setError(e.message ?? String(e));
    }
  }, [connection, publicKey, portfolioKey]);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 4000);
    return () => clearInterval(id);
  }, [refresh]);

  return { state, refresh, error, portfolioKey };
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {sub && <div className="stat-sub">{sub}</div>}
    </div>
  );
}

export default function App() {
  const { state, refresh, error, portfolioKey } = useChain();
  const [tab, setTab] = useState<"vault" | "trade" | "how">("vault");
  const { connection } = useConnection();
  const wallet = useWallet();
  const [busy, setBusy] = useState<string>();
  const [toast, setToast] = useState<{ ok: boolean; text: string; sig?: string }>();

  const send = useCallback(
    async (label: string, ixs: TransactionInstruction[]) => {
      if (!wallet.publicKey) return;
      setBusy(label);
      try {
        // Percolator trades can use several hundred thousand compute units.
        const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), ...ixs);
        const sig = await wallet.sendTransaction(tx, connection, { skipPreflight: false });
        await connection.confirmTransaction(sig, "confirmed");
        setToast({ ok: true, text: `${label}: confirmed`, sig });
        await refresh();
      } catch (e: any) {
        const logs: string[] = e?.logs ?? [];
        const code = logs.map(l => l.match(/custom program error: (0x[0-9a-f]+)/)?.[1]).find(Boolean);
        setToast({ ok: false, text: `${label} failed${code ? ` (${explainError(code)})` : ""}: ${e.message ?? e}` });
      } finally {
        setBusy(undefined);
      }
    },
    [wallet, connection, refresh],
  );

  return (
    <div className="page">
      <header className="top">
        <div className="brand">
          <span className="logo" aria-hidden>◈</span>
          <div>
            <div className="brand-name">Percolator Vaults</div>
            <div className="brand-sub">Permissionless liquidity for Percolator perps · devnet</div>
          </div>
        </div>
        <nav className="tabs">
          {(["vault", "trade", "how"] as const).map(t => (
            <button key={t} className={tab === t ? "tab active" : "tab"} onClick={() => setTab(t)}>
              {t === "vault" ? "Provide liquidity" : t === "trade" ? "Trade SOL-PERP" : "How it works"}
            </button>
          ))}
        </nav>
        <WalletMultiButton />
      </header>

      {error && <div className="banner warn">RPC error: {error}</div>}
      {!state ? (
        <div className="loading">Loading vault…</div>
      ) : tab === "vault" ? (
        <VaultTab state={state} send={send} busy={busy} />
      ) : tab === "trade" ? (
        <TradeTab state={state} send={send} busy={busy} portfolioKey={portfolioKey} />
      ) : (
        <HowTab />
      )}

      {toast && (
        <div className={toast.ok ? "toast ok" : "toast err"} onClick={() => setToast(undefined)}>
          {toast.text}
          {toast.sig && (
            <a href={explorer("tx", toast.sig)} target="_blank" rel="noreferrer"> · view</a>
          )}
        </div>
      )}
      <footer className="foot">
        Unaudited experimental software on devnet. Vault <a href={explorer("address", C.VAULT.toString())} target="_blank" rel="noreferrer">{short(C.VAULT)}</a> · program{" "}
        <a href={explorer("address", C.VAULT_PROGRAM.toString())} target="_blank" rel="noreferrer">{short(C.VAULT_PROGRAM)}</a> · Percolator{" "}
        <a href={explorer("address", C.PERCOLATOR.toString())} target="_blank" rel="noreferrer">{short(C.PERCOLATOR)}</a>
      </footer>
    </div>
  );
}

const ERRORS: Record<string, string> = {
  "0x5608": "vault not active", "0x560a": "claim your previous request first", "0x560b": "nothing to claim",
  "0x560c": "epoch not over", "0x560d": "epoch not settled yet", "0x560e": "vault not flat yet", "0x5610": "amount is zero",
  "0x1": "not enough test USDC: use Get test USDC", "0x15": "market busy: try again in a few seconds", "0x13": "market state stale: try again", "0xe": "not enough margin",
};
const explainError = (code: string) => ERRORS[code] ?? code;

type TabProps = { state: C.State; send: (label: string, ixs: TransactionInstruction[]) => Promise<void>; busy?: string };

function VaultTab({ state, send, busy }: TabProps) {
  const { publicKey } = useWallet();
  const [amount, setAmount] = useState("100");
  const [shares, setShares] = useState("");
  const v = state.vault;
  const nav = state.lp.capital + (state.lp.pnl > 0n ? state.lp.pnl : 0n) + state.buffer - v.reserved - v.pendingDeposit;
  const sharePrice = state.shareSupply > 0n ? Number(nav) / C.USDC / (Number(state.shareSupply) / 10 ** C.SHARE_DECIMALS) : 1;
  const epochEnd = v.epochStart + v.epochLen;
  const slotsLeft = epochEnd > state.slot ? epochEnd - state.slot : 0n;
  const reduceOnly = slotsLeft === 0n && (v.pendingDeposit > 0n || v.pendingWithdraw > 0n);
  const t = state.ticket;
  const claimable = t && (t.deposit > 0n || t.withdraw > 0n) && t.epoch < v.epoch;
  const pendingMine = t && (t.deposit > 0n || t.withdraw > 0n) && t.epoch === v.epoch;
  const myValue = Number(state.userShares) / 10 ** C.SHARE_DECIMALS * sharePrice;

  const notEnough = BigInt(Math.floor(Number(amount || 0) * C.USDC)) > state.userCollateral;
  const deposit = () => {
    if (!publicKey) return;
    const atoms = BigInt(Math.floor(Number(amount) * C.USDC));
    if (atoms <= 0n) return;
    const ixs: TransactionInstruction[] = [
      createAssociatedTokenAccountIdempotentInstruction(publicKey, C.ata(publicKey, C.MINT), publicKey, C.MINT),
    ];
    if (claimable) ixs.push(...claimIxs(publicKey, t!.epoch));
    ixs.push(C.requestDeposit(publicKey, atoms));
    send("Deposit request", ixs);
  };
  const withdraw = () => {
    if (!publicKey) return;
    const s = shares ? BigInt(Math.floor(Number(shares) * 10 ** C.SHARE_DECIMALS)) : state.userShares;
    const ixs: TransactionInstruction[] = [];
    if (claimable) ixs.push(...claimIxs(publicKey, t!.epoch));
    ixs.push(C.requestWithdraw(publicKey, s));
    send("Withdraw request", ixs);
  };

  return (
    <main className="grid">
      <section className="card span2">
        <div className="card-head">
          <h2>SOL-PERP liquidity vault</h2>
          <span className={v.status === 1 ? "pill good" : "pill"}>{v.status === 1 ? "Active" : v.status === 2 ? "Wound down" : "Pending listing"}</span>
        </div>
        <p className="muted">
          This vault listed its own SOL/USD perpetual on Percolator, priced by Pyth. It is the counterparty to traders and
          collects the market's trading fees. No one holds an admin key: every parameter was fixed at creation, and every
          maintenance step can be run by anyone.
        </p>
        <div className="stats">
          <Stat label="Net asset value" value={`$${fmtUsd(nav)}`} sub={`${fmtShares(state.shareSupply)} shares`} />
          <Stat label="Share price" value={`$${sharePrice.toFixed(4)}`} sub="per share" />
          <Stat label="Fees harvested" value={`$${fmtUsd(v.feesHarvested)}`} sub={`insurance $${fmtUsd(state.asset.insurance)}`} />
          <Stat label="Vault position" value={`${fmtSol(v.inventory)} SOL`} sub={`limit ±${fmtSol(v.maxInventory)} SOL`} />
          <Stat label="SOL price" value={`$${fmtPrice(state.asset.price)}`} sub="Percolator mark (Pyth)" />
          <Stat label={`Epoch ${v.epoch}`} value={slotsLeft > 0n ? `${Math.ceil(Number(slotsLeft) * SLOT_SECONDS / 60)} min left` : reduceOnly ? "Settling" : "Open"} sub={`${fmtUsd(v.pendingDeposit)} USDC in · ${fmtShares(v.pendingWithdraw)} shares out`} />
        </div>
      </section>

      <section className="card span2">
        <h3>Your position</h3>
        {!publicKey ? (
          <p className="muted">Connect a wallet to deposit.</p>
        ) : (
          <>
            <div className="row"><span>Shares</span><b>{fmtShares(state.userShares)}</b></div>
            <div className="row"><span>Value</span><b>${myValue.toFixed(2)}</b></div>
            <div className="row"><span>Wallet USDC (test)</span><b>{fmtUsd(state.userCollateral)}</b></div>
            {pendingMine && (
              <div className="note">
                Queued for epoch {String(t!.epoch)}: {t!.deposit > 0n && `${fmtUsd(t!.deposit)} USDC deposit`} {t!.withdraw > 0n && `${fmtShares(t!.withdraw)} shares withdrawal`}. It settles at one price when the epoch rolls.
              </div>
            )}
            {claimable && (
              <button className="btn primary wide" disabled={!!busy} onClick={() => send("Claim", claimIxs(publicKey, t!.epoch))}>
                Claim epoch {String(t!.epoch)} result
              </button>
            )}
            <Faucet />
          </>
        )}
      </section>

      <section className="card">
        <h3>Deposit</h3>
        <label className="field">
          <span>USDC</span>
          <input value={amount} onChange={e => setAmount(e.target.value)} inputMode="decimal" />
        </label>
        <button className="btn primary wide" disabled={!publicKey || !!busy || v.status !== 1 || notEnough} onClick={deposit}>
          {busy === "Deposit request" ? "Sending…" : "Request deposit"}
        </button>
        {publicKey && notEnough && <p className="small down">You have {fmtUsd(state.userCollateral)} test USDC. Use "Get test USDC" first.</p>}
        <p className="muted small">Priced at the next epoch roll, together with every other request, so nobody can trade against a stale share price.</p>
      </section>

      <section className="card">
        <h3>Withdraw</h3>
        <label className="field">
          <span>Shares</span>
          <input value={shares} placeholder={fmtShares(state.userShares)} onChange={e => setShares(e.target.value)} inputMode="decimal" />
        </label>
        <button className="btn wide" disabled={!publicKey || !!busy || state.userShares === 0n || v.status !== 1} onClick={withdraw}>
          {busy === "Withdraw request" ? "Sending…" : "Request withdrawal"}
        </button>
        <p className="muted small">After the epoch ends the vault only takes trades that shrink its position. If a trader holds a position to block exits, anyone can make the vault close it one epoch later.</p>
      </section>
    </main>
  );
}

function claimIxs(user: PublicKey, epoch: bigint) {
  return [
    createAssociatedTokenAccountIdempotentInstruction(user, C.ata(user, C.SHARE_MINT), user, C.SHARE_MINT),
    createAssociatedTokenAccountIdempotentInstruction(user, C.ata(user, C.MINT), user, C.MINT),
    C.claim(user, epoch),
  ];
}

function Faucet() {
  const { publicKey } = useWallet();
  const [msg, setMsg] = useState<string>();
  const drip = async () => {
    if (!publicKey) return;
    setMsg("Requesting…");
    try {
      const r = await fetch("/api/faucet", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ address: publicKey.toString() }) });
      const j = await r.json();
      setMsg(r.ok ? `Sent ${j.usdc} test USDC${j.sol ? ` and ${j.sol} SOL` : ""}` : j.error);
    } catch (e: any) {
      setMsg(e.message);
    }
  };
  return (
    <div className="faucet">
      <button className="btn ghost wide" onClick={drip}>Get test USDC</button>
      {msg && <div className="muted small">{msg}</div>}
    </div>
  );
}

function TradeTab({ state, send, busy, portfolioKey }: TabProps & { portfolioKey?: PublicKey }) {
  const { publicKey } = useWallet();
  const [size, setSize] = useState("1");
  const [margin, setMargin] = useState("100");
  const p = state.portfolio;
  const a = state.asset;
  const notional = Number(size || 0) * Number(a.price) / 1e6;

  const createPortfolio = async () => {
    if (!publicKey || !portfolioKey) return;
    send("Create trading account", [
      SystemProgram.createAccountWithSeed({
        fromPubkey: publicKey,
        basePubkey: publicKey,
        seed: C.PORTFOLIO_SEED,
        newAccountPubkey: portfolioKey,
        lamports: await rentFor(C.PORTFOLIO_LEN),
        space: C.PORTFOLIO_LEN,
        programId: C.PERCOLATOR,
      }),
      C.initPortfolio(publicKey, portfolioKey),
    ]);
  };
  const { connection } = useConnection();
  const rentFor = (len: number) => connection.getMinimumBalanceForRentExemption(len);

  const trade = (sign: 1n | -1n) => {
    if (!publicKey || !portfolioKey || !p) return;
    const q = BigInt(Math.round(Number(size) * 1e6)) * sign;
    send(sign > 0n ? "Long" : "Short", [C.tradeAgainstVault(publicKey, portfolioKey, p, state.lp, a, q)]);
  };
  const close = () => {
    if (!publicKey || !portfolioKey || !p || p.position === 0n) return;
    send("Close", [C.tradeAgainstVault(publicKey, portfolioKey, p, state.lp, a, -p.position)]);
  };

  return (
    <main className="grid">
      <section className="card span2">
        <div className="card-head">
          <h2>SOL-PERP</h2>
          <span className="price">${fmtPrice(a.price)}</span>
        </div>
        <div className="stats">
          <Stat label="Open interest" value={`${fmtSol(a.oiLong)} SOL`} sub="long = short" />
          <Stat label="Counterparty" value="Vault" sub={`takes up to ${fmtSol(state.vault.maxInventory)} SOL net`} />
          <Stat label="Max leverage" value="10×" sub="10% initial margin" />
          <Stat label="Trading fee" value="0.05%" sub="funds the vault's insurance" />
        </div>
      </section>
      <section className="card">
        <h3>Your account</h3>
        {!publicKey ? (
          <p className="muted">Connect a wallet to trade.</p>
        ) : !p ? (
          <>
            <p className="muted">Trading needs a Percolator portfolio account (about 0.067 devnet SOL of rent, yours to reclaim).</p>
            <button className="btn primary wide" disabled={!!busy} onClick={createPortfolio}>Create trading account</button>
          </>
        ) : (
          <>
            <div className="row"><span>Margin</span><b>${fmtUsd(p.capital)}</b></div>
            <div className="row"><span>Position</span><b className={p.position > 0n ? "up" : p.position < 0n ? "down" : ""}>{fmtSol(p.position)} SOL</b></div>
            <div className="row"><span>Realized PnL</span><b>${fmtUsd(p.pnl)}</b></div>
            <label className="field"><span>USDC</span><input value={margin} onChange={e => setMargin(e.target.value)} /></label>
            <div className="btns">
              <button className="btn" disabled={!!busy} onClick={() => send("Add margin", [C.percDeposit(publicKey, portfolioKey!, p, BigInt(Math.floor(Number(margin) * C.USDC)))])}>Add margin</button>
              <button className="btn" disabled={!!busy || p.position !== 0n} onClick={() => send("Withdraw margin", [C.percWithdraw(publicKey, portfolioKey!, p, BigInt(Math.floor(Number(margin) * C.USDC)))])}>Withdraw</button>
            </div>
            <Faucet />
          </>
        )}
      </section>
      <section className="card">
        <h3>Trade</h3>
        <label className="field"><span>Size (SOL)</span><input value={size} onChange={e => setSize(e.target.value)} inputMode="decimal" /></label>
        <div className="muted small">Notional ≈ ${notional.toFixed(2)} · margin needed ≈ ${(notional / 10).toFixed(2)}</div>
        <div className="btns">
          <button className="btn long" disabled={!p || !!busy} onClick={() => trade(1n)}>Long</button>
          <button className="btn short" disabled={!p || !!busy} onClick={() => trade(-1n)}>Short</button>
        </div>
        <button className="btn ghost wide" disabled={!p || p.position === 0n || !!busy} onClick={close}>Close position</button>
        <p className="muted small">Trades settle at Percolator's mark price. If the vault is at its limit or settling an epoch, it may fill less than you ask, or nothing.</p>
      </section>
    </main>
  );
}

function HowTab() {
  const steps = useMemo(
    () => [
      ["Anyone creates a vault", "Parameters are fixed forever at creation: spread, fill and inventory limits, epoch length, price feed, insurance floor. There is no admin instruction in the program."],
      ["The vault lists its own market", "It activates a new Percolator asset and names itself as that asset's admin, insurance operator, backing authority and oracle authority. No person holds any of those keys; prices come from Pyth."],
      ["The vault is the counterparty", "The vault program is also the matcher Percolator calls on every trade, with inventory caps enforced on-chain. Trades settle at Percolator's mark price."],
      ["Fees become yield", "Trading fees land in the asset's insurance. Anyone can harvest what is above the fixed floor into the vault; the floor stays to protect traders."],
      ["Epochs keep share prices honest", "Deposits and withdrawals queue during an epoch and settle together at one price once the vault is flat. Withdrawals are priced conservatively and deposits pay for unharvested fees, so neither side can dilute the other."],
      ["Nobody can lock the exits", "After an epoch, the vault only takes trades that shrink its position. If someone holds a position to block exits, anyone can make the vault close it through Percolator one epoch later."],
      ["If the market shuts down", "When the market is resolved, anyone can settle the vault's position; queued deposits are refunded and shares redeem pro rata."],
    ],
    [],
  );
  return (
    <main className="grid">
      <section className="card span2">
        <h2>How it works</h2>
        <ol className="steps">
          {steps.map(([t, d]) => (
            <li key={t}><b>{t}.</b> {d}</li>
          ))}
        </ol>
        <p className="muted small">Built on Anatoly Yakovenko's Percolator risk engine. The vault program has no admin instruction and is tested against the production Percolator binary, including attack scenarios. On devnet the program is still upgradeable by its deployer; a mainnet deployment would burn that authority. It has not been audited.</p>
      </section>
    </main>
  );
}
