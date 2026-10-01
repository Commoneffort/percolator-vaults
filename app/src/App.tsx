import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useConnection, useWallet } from "@solana/wallet-adapter-react";
import { WalletMultiButton } from "@solana/wallet-adapter-react-ui";
import { ComputeBudgetProgram, PublicKey, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction } from "@solana/spl-token";
import * as C from "./chain";
import Docs from "./Docs";

const num = (x: bigint, scale: number) => Number(x) / scale;
const fmt = (x: number, dp = 2) => x.toLocaleString(undefined, { minimumFractionDigits: dp, maximumFractionDigits: dp });
const usd = (atoms: bigint, dp = 2) => `$${fmt(num(atoms, C.USDC), dp)}`;
const priceDp = (p: number) => (p >= 1000 ? 2 : p >= 1 ? 3 : 5);
const px = (e6: bigint) => { const p = num(e6, 1e6); return `$${fmt(p, priceDp(p))}`; };
const units = (q: bigint) => fmt(num(q, Number(C.UNIT)), 4).replace(/\.?0+$/, "") || "0";
const shares = (s: bigint) => fmt(num(s, 10 ** C.SHARE_DECIMALS), 2);
const short = (k: PublicKey | string) => { const s = k.toString(); return `${s.slice(0, 4)}…${s.slice(-4)}`; };
const explorer = (what: "address" | "tx", id: string) => `https://explorer.solana.com/${what}/${id}?cluster=devnet`;
/** Vault size cap in position units: `bps` of NAV in notional at the current price. */
const capUnits = (nav: bigint, bps: number, priceE6: bigint) => (priceE6 > 0n ? (nav * BigInt(bps) / 10_000n) * C.UNIT / priceE6 : 0n);
const ago = (sec: number) => (sec < 60 ? `${Math.max(1, Math.round(sec))}s ago` : sec < 3600 ? `${Math.round(sec / 60)}m ago` : `${Math.round(sec / 3600)}h ago`);

// ---------------------------------------------------------------- routing

function useRoute(): [string[], (to: string) => void] {
  const parse = () => window.location.hash.replace(/^#\/?/, "").split("/").filter(Boolean);
  const [route, setRoute] = useState(parse);
  useEffect(() => {
    const on = () => setRoute(parse());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  return [route, (to: string) => { window.location.hash = to; window.scrollTo(0, 0); }];
}

// ---------------------------------------------------------------- transactions

type Toast = { ok: boolean; text: string; sig?: string };
type Send = (label: string, ixs: TransactionInstruction[]) => Promise<string | undefined>;

const ERRORS: Record<string, string> = {
  "0x1": "not enough test USDC: use Get test USDC", "0x5608": "vault not active", "0x560a": "claim your previous request first",
  "0x560b": "nothing to claim", "0x560c": "epoch not over", "0x560d": "epoch not settled yet", "0x560e": "vault not flat yet",
  "0x5610": "amount is zero", "0x5611": "unknown asset", "0x15": "market busy: try again in a few seconds",
  "0x13": "market state stale: try again", "0xe": "not enough margin", "0x12": "asset generation changed: retry",
};

function useSend(onDone: () => void): [Send, string | undefined, Toast | undefined, () => void] {
  const { connection } = useConnection();
  const wallet = useWallet();
  const [busy, setBusy] = useState<string>();
  const [toast, setToast] = useState<Toast>();
  const send = useCallback<Send>(
    async (label, ixs) => {
      if (!wallet.publicKey) return;
      setBusy(label);
      try {
        // Percolator instructions can use several hundred thousand compute units.
        const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ...ixs);
        const sig = await wallet.sendTransaction(tx, connection);
        await connection.confirmTransaction(sig, "confirmed");
        setToast({ ok: true, text: `${label}: confirmed`, sig });
        onDone();
        return sig;
      } catch (e: any) {
        const logs: string[] = e?.logs ?? [];
        const code = logs.map(l => l.match(/custom program error: (0x[0-9a-f]+)/)?.[1]).find(Boolean) ?? String(e?.message ?? "").match(/custom program error: (0x[0-9a-f]+)/)?.[1];
        setToast({ ok: false, text: `${label} failed${code ? `: ${ERRORS[code] ?? code}` : `: ${e?.message ?? e}`}` });
      } finally {
        setBusy(undefined);
      }
    },
    [wallet, connection, onDone],
  );
  return [send, busy, toast, () => setToast(undefined)];
}

// ---------------------------------------------------------------- app shell

export default function App() {
  const [route, go] = useRoute();
  const [tick, setTick] = useState(0);
  const refresh = useCallback(() => setTick(t => t + 1), []);
  const [send, busy, toast, clearToast] = useSend(refresh);
  const page = route[0] ?? "";
  const { connection } = useConnection();
  useEffect(() => { C.measureSlotSeconds(connection).then(refresh); }, [connection, refresh]);

  return (
    <div className="page">
      <header className="top">
        <a className="brand" href="#/">
          <span className="logo" aria-hidden>◈</span>
          <div>
            <div className="brand-name">Percolator Vaults</div>
            <div className="brand-sub">Keyless perp markets with liquidity built in · devnet</div>
          </div>
        </a>
        <nav className="tabs">
          <a className={page === "" || page === "m" ? "tab active" : "tab"} href="#/">Markets</a>
          <a className={page === "launch" ? "tab active" : "tab"} href="#/launch">Open a market</a>
          <a className={page === "leaderboard" ? "tab active" : "tab"} href="#/leaderboard">Leaderboard</a>
          <a className={page === "how" || page === "docs" ? "tab active" : "tab"} href="#/docs">Docs</a>
        </nav>
        <WalletMultiButton />
      </header>

      {page === "m" && route[1] ? (
        <MarketPage vaultKey={route[1]} send={send} busy={busy} tick={tick} />
      ) : page === "launch" ? (
        <LaunchPage key={route[1] ?? ""} send={send} busy={busy} go={go} initial={route[1]} />
      ) : page === "how" || page === "docs" ? (
        <Docs />
      ) : page === "leaderboard" ? (
        <LeaderboardPage />
      ) : (
        <MarketsPage go={go} tick={tick} />
      )}

      {toast && (
        <div className={toast.ok ? "toast ok" : "toast err"} onClick={clearToast}>
          {toast.text}
          {toast.sig && <a href={explorer("tx", toast.sig)} target="_blank" rel="noreferrer"> · view</a>}
        </div>
      )}
      <footer className="foot">
        Unaudited experimental software on Solana devnet · vault program{" "}
        <a href={explorer("address", C.VAULT_PROGRAM.toString())} target="_blank" rel="noreferrer">{short(C.VAULT_PROGRAM)}</a> · Percolator{" "}
        <a href={explorer("address", C.PERCOLATOR.toString())} target="_blank" rel="noreferrer">{short(C.PERCOLATOR)}</a> ·{" "}
        <a href="https://github.com/Commoneffort/percolator-vaults" target="_blank" rel="noreferrer">source</a>
      </footer>
    </div>
  );
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

function Icon({ symbol }: { symbol: string }) {
  const hue = [...symbol].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7);
  return <span className="coin" style={{ background: `hsl(${hue} 55% 42%)` }}>{symbol.slice(0, 1)}</span>;
}

// ---------------------------------------------------------------- markets directory

type Card = { v: C.Vault; asset: C.Asset; nav: bigint };

function useCards(tick: number) {
  const { connection } = useConnection();
  const [cards, setCards] = useState<Card[]>();
  const [err, setErr] = useState<string>();
  useEffect(() => {
    let live = true;
    const load = async () => {
      try {
        const [vaults, m] = await Promise.all([C.listVaults(connection), C.fetchMarket(connection)]);
        const lps = await connection.getMultipleAccountsInfo(vaults.flatMap(v => [v.lpPortfolio, v.buffer]), "confirmed");
        const out = vaults.map((v, i) => {
          const lp = lps[2 * i] ? C.decodePortfolio(new Uint8Array(lps[2 * i]!.data), v.assetIndex) : undefined;
          const b = lps[2 * i + 1];
          const buf = b ? new DataView(b.data.buffer, b.data.byteOffset).getBigUint64(64, true) : 0n;
          const nav = (lp ? lp.capital + (lp.pnl > 0n ? lp.pnl : 0n) : 0n) + buf - v.reserved - v.pendingDeposit;
          return { v, asset: v.status === 0 ? ({} as C.Asset) : C.decodeAsset(m.data, v.assetIndex), nav };
        });
        out.sort((a, b) => (b.nav > a.nav ? 1 : b.nav < a.nav ? -1 : 0));
        if (live) { setCards(out); setErr(undefined); }
      } catch (e: any) { if (live) setErr(e.message); }
    };
    load();
    const id = setInterval(load, 8000);
    return () => { live = false; clearInterval(id); };
  }, [connection, tick]);
  return { cards, err };
}

function MarketCard({ c, go }: { c: Card; go: (to: string) => void }) {
  const sym = c.v.feed!.symbol;
  return (
    <button className="market-card" onClick={() => go(`/m/${c.v.key.toString()}`)}>
      <div className="mc-head">
        <Icon symbol={sym} />
        <div>
          <div className="mc-sym">{sym}-PERP</div>
          <div className="muted small">{c.v.feed!.name} · vault {short(c.v.key)}</div>
        </div>
        <div className="mc-price">{px(c.asset.price)}</div>
      </div>
      <div className="mc-row"><span>Liquidity</span><b>{usd(c.nav, 0)}</b></div>
      <div className="mc-row"><span>Open interest</span><b>{units(c.asset.oiLong)} {sym}</b></div>
      <div className="mc-row"><span>Fees</span><b>{usd(c.v.feesHarvested + c.asset.insurance)}</b></div>
      <div className="mc-row"><span>Epoch</span><b>{String(c.v.epoch)} · {Math.round(Number(c.v.epochLen) * C.slotSeconds() / 60)} min</b></div>
    </button>
  );
}

function MarketsPage({ go, tick }: { go: (to: string) => void; tick: number }) {
  const { cards, err } = useCards(tick);
  const canon = new Map((cards ?? []).filter(c => c.v.canonical).map(c => [c.v.feed!.symbol, c]));
  const legacy = (cards ?? []).filter(c => !c.v.canonical);
  const live = [...canon.values()].filter(c => c.v.status === 1);
  const tvl = (cards ?? []).reduce((a, c) => a + c.nav, 0n);
  const fills = (cards ?? []).reduce((a, c) => a + c.v.totalFills, 0n);
  const fees = (cards ?? []).reduce((a, c) => a + c.v.feesHarvested + (c.asset.insurance ?? 0n), 0n);

  return (
    <main>
      <section className="hero">
        <h1>One perpetual market per asset.<br /><span className="grad">One shared pool. No keys.</span></h1>
        <p className="muted">
          Anyone can open the market for a Pyth price feed; after that, everyone trades the same market and every liquidity
          provider shares the same vault. The vault lists the market on Anatoly Yakovenko's Percolator engine, owns every key to
          it, and is the counterparty to every trade. Depositors earn its fees. Nobody can pause, change or drain it.
        </p>
        <div className="hero-cta">
          <button className="btn primary" onClick={() => go("/launch")}>Open a market</button>
          <button className="btn ghost" onClick={() => go("/leaderboard")}>Leaderboard</button>
          <button className="btn ghost" onClick={() => go("/docs")}>Read the docs</button>
        </div>
        <div className="stats four">
          <Stat label="Markets" value={cards ? `${live.length} / ${C.FEEDS.length}` : "…"} sub="open / possible" />
          <Stat label="Liquidity" value={usd(tvl, 0)} sub="in all vaults" />
          <Stat label="Fills" value={String(fills)} sub="trades matched by vaults" />
          <Stat label="Fees earned" value={usd(fees)} sub="harvested + pending" />
        </div>
      </section>
      {err && <div className="banner warn">RPC error: {err}</div>}
      <div className="market-grid">
        {!cards ? <div className="loading">Loading markets…</div> : C.FEEDS.map(f => {
          const c = canon.get(f.symbol);
          if (c && c.v.status === 1) return <MarketCard key={f.symbol} c={c} go={go} />;
          return (
            <button key={f.symbol} className="market-card launch-card" onClick={() => go(`/launch/${f.symbol}`)}>
              <Icon symbol={f.symbol} />
              <div className="mc-sym">{f.symbol}-PERP</div>
              <div className="muted small">{c ? "Created, waiting to be listed. Anyone can finish it." : `Not open yet. Open the ${f.name} market.`}</div>
            </button>
          );
        })}
      </div>
      {legacy.length > 0 && (
        <>
          <h3 className="section-title">Earlier vaults</h3>
          <p className="muted small">Created before one-market-per-asset. They still trade and settle normally, and their depositors can withdraw at any epoch.</p>
          <div className="market-grid">{legacy.map(c => <MarketCard key={c.v.key.toString()} c={c} go={go} />)}</div>
        </>
      )}
    </main>
  );
}

// ---------------------------------------------------------------- market page

function useVault(vaultKey: PublicKey, tick: number) {
  const { connection } = useConnection();
  const { publicKey } = useWallet();
  const [state, setState] = useState<C.State>();
  const [err, setErr] = useState<string>();
  const [history, setHistory] = useState<{ t: number; p: number }[]>([]);
  const load = useCallback(async () => {
    try {
      const s = await C.fetchVault(connection, vaultKey, publicKey ?? undefined);
      setState(s);
      setErr(undefined);
      const p = num(s.asset.price, 1e6);
      setHistory(h => (h.length && h[h.length - 1].p === p && Date.now() - h[h.length - 1].t < 30_000 ? h : [...h.slice(-240), { t: Date.now(), p }]));
    } catch (e: any) { setErr(e.message); }
  }, [connection, vaultKey.toString(), publicKey?.toString()]);
  useEffect(() => { setHistory([]); }, [vaultKey.toString()]);
  useEffect(() => { load(); const id = setInterval(load, 6000); return () => clearInterval(id); }, [load, tick]);
  return { state, err, history, reload: load };
}

function MarketPage({ vaultKey, send, busy, tick }: { vaultKey: string; send: Send; busy?: string; tick: number }) {
  const key = useMemo(() => { try { return new PublicKey(vaultKey); } catch { return undefined; } }, [vaultKey]);
  if (!key) return <div className="banner warn">Invalid market address.</div>;
  return <MarketInner vaultKey={key} send={send} busy={busy} tick={tick} />;
}

function MarketInner({ vaultKey, send, busy, tick }: { vaultKey: PublicKey; send: Send; busy?: string; tick: number }) {
  const { state, err, history, reload } = useVault(vaultKey, tick);
  const [tab, setTab] = useState<"trade" | "lp">("trade");
  const wrapped: Send = async (l, ixs) => { const r = await send(l, ixs); reload(); return r; };
  if (err && !state) return <div className="banner warn">{err}</div>;
  if (!state) return <div className="loading">Loading market…</div>;
  const v = state.vault;
  const sym = v.feed?.symbol ?? "?";
  const nav = C.navOf(state);
  const age = state.pyth ? Date.now() / 1000 - state.pyth.publish : undefined;
  return (
    <main className="grid">
      <section className="card span2">
        <div className="card-head">
          <div className="mc-head">
            <Icon symbol={sym} />
            <div>
              <h2>{sym}-PERP</h2>
              <div className="muted small">
                Pyth {v.feed?.name} feed{age !== undefined && ` · updated ${ago(age)}`} · vault{" "}
                <a href={explorer("address", v.key.toString())} target="_blank" rel="noreferrer">{short(v.key)}</a>
                {v.canonical && <> · opened by <a href={explorer("address", v.creator.toString())} target="_blank" rel="noreferrer">{short(v.creator)}</a></>}
              </div>
            </div>
          </div>
          <div className="price">{px(state.asset.price)}</div>
        </div>
        <Sparkline points={history} />
        <div className="stats">
          <Stat label="Vault liquidity" value={usd(nav)} sub={`${shares(state.shareSupply)} shares`} />
          <Stat label="Vault position" value={`${units(v.inventory)} ${sym}`} sub={v.positionNavBps
            ? `limit ±${units(capUnits(nav, v.positionNavBps, state.asset.price))} (3× NAV), ≤${units(capUnits(nav, v.fillNavBps, state.asset.price))} per fill`
            : `limit ±${units(v.maxInventory)}, ≤${units(v.maxFill)} per fill`} />
          <Stat label="Open interest" value={`${units(state.asset.oiLong)} ${sym}`} sub="long = short" />
          <Stat label="Fees" value={usd(v.feesHarvested + state.asset.insurance)} sub={`${usd(v.feesHarvested)} harvested · floor ${usd(v.insuranceFloor, 0)}`} />
          <DepthStat state={state} />
          <EpochStat state={state} />
        </div>
      </section>
      <OpenerPanel state={state} send={wrapped} busy={busy} />
      <div className="span2 subtabs">
        <button className={tab === "trade" ? "tab active" : "tab"} onClick={() => setTab("trade")}>Trade</button>
        <button className={tab === "lp" ? "tab active" : "tab"} onClick={() => setTab("lp")}>Provide liquidity</button>
      </div>
      {tab === "trade" ? <TradePanel state={state} send={wrapped} busy={busy} /> : <LiquidityPanel state={state} send={wrapped} busy={busy} />}
      <Activity vault={v} tick={tick} />
    </main>
  );
}

function DepthStat({ state }: { state: C.State }) {
  const d = C.depth(state);
  const sym = state.vault.feed?.symbol ?? "";
  const usdOf = (q: bigint) => fmt(num(q, 1e6) * num(state.asset.price, 1e6), 0);
  return <Stat label="Max trade now" value={`${units(d.long)} / ${units(d.short)} ${sym}`} sub={`long $${usdOf(d.long)} · short $${usdOf(d.short)} · 10×`} />;
}

function OpenerPanel({ state, send, busy }: { state: C.State; send: Send; busy?: string }) {
  const { publicKey } = useWallet();
  const v = state.vault;
  if (!v.canonical || !publicKey || !publicKey.equals(v.creator)) return null;
  return (
    <section className="card span2 opener">
      <div className="card-head">
        <div>
          <h3>You opened this market</h3>
          <p className="muted small">You earn {C.OPENER_FEE_BPS / 100}% of every fee this market harvests, forever. You have no other powers over it.</p>
        </div>
        <div className="opener-num">
          <div className="stat-value">{usd(v.openerFeesOwed)}</div>
          <div className="muted small">claimable · {usd(v.openerFeesTotal)} earned in total</div>
        </div>
      </div>
      <button className="btn primary" disabled={!!busy || v.openerFeesOwed === 0n} onClick={() => send("Claim opener fees", [
        createAssociatedTokenAccountIdempotentInstruction(publicKey, C.ata(publicKey, C.MINT), publicKey, C.MINT),
        C.claimOpenerFees(v, publicKey),
      ])}>Claim opener fees</button>
    </section>
  );
}

function EpochStat({ state }: { state: C.State }) {
  const v = state.vault;
  const end = v.epochStart + v.epochLen;
  const left = end > state.slot ? end - state.slot : 0n;
  const settling = left === 0n && (v.pendingDeposit > 0n || v.pendingWithdraw > 0n);
  return (
    <Stat
      label={`Epoch ${v.epoch}`}
      value={left > 0n ? `${Math.max(1, Math.ceil(Number(left) * C.slotSeconds() / 60))} min left` : settling ? "Settling" : "Rolling"}
      sub={`${usd(v.pendingDeposit)} in · ${shares(v.pendingWithdraw)} shares out`}
    />
  );
}

function Sparkline({ points }: { points: { t: number; p: number }[] }) {
  if (points.length < 2) return <div className="spark empty muted small">Price chart builds live while this page is open.</div>;
  const w = 1000, h = 120;
  const ps = points.map(x => x.p);
  const lo = Math.min(...ps), hi = Math.max(...ps);
  const span = hi - lo || hi * 0.001 || 1;
  const t0 = points[0].t, t1 = points[points.length - 1].t;
  const xy = points.map(pt => [((pt.t - t0) / (t1 - t0 || 1)) * w, h - 8 - ((pt.p - lo) / span) * (h - 16)]);
  const d = xy.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const up = ps[ps.length - 1] >= ps[0];
  return (
    <svg className="spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" role="img" aria-label="price chart">
      <path d={`${d} L${w},${h} L0,${h} Z`} className={up ? "area up" : "area down"} />
      <path d={d} className={up ? "line up" : "line down"} vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

function Activity({ vault, tick }: { vault: C.Vault; tick: number }) {
  const { connection } = useConnection();
  const [rows, setRows] = useState<{ sig: string; when: number; what: string }[]>();
  const seen = useRef(new Map<string, string>());
  useEffect(() => {
    let live = true;
    const load = async () => {
      try {
        const sigs = (await connection.getSignaturesForAddress(vault.key, { limit: 25 }, "confirmed")).filter(s => !s.err);
        // A few new transactions per pass, one request each (public RPCs refuse batches).
        for (const s of sigs.filter(s => !seen.current.has(s.signature)).slice(0, 4)) {
          const tx = await connection.getTransaction(s.signature, { maxSupportedTransactionVersion: 0, commitment: "confirmed" });
          seen.current.set(s.signature, describe(tx, vault));
        }
        // Price updates (the executor moving the mark every few seconds) are not activity.
        const shown = sigs.filter(s => seen.current.has(s.signature) && seen.current.get(s.signature) !== "").slice(0, 12);
        if (live) setRows(shown.map(s => ({ sig: s.signature, when: s.blockTime ?? 0, what: seen.current.get(s.signature)! })));
      } catch { /* public RPC may rate-limit history; keep the last list */ }
    };
    load();
    const id = setInterval(load, 15000);
    return () => { live = false; clearInterval(id); };
  }, [connection, vault.key.toString(), tick]);
  return (
    <section className="card span2">
      <h3>Activity</h3>
      {!rows ? <div className="muted small">Loading…</div> : rows.length === 0 ? <div className="muted small">No activity yet.</div> : (
        <div className="activity">
          {rows.map(r => (
            <a key={r.sig} className="act-row" href={explorer("tx", r.sig)} target="_blank" rel="noreferrer">
              <span>{r.what}</span>
              <span className="muted small">{r.when ? ago(Date.now() / 1000 - r.when) : ""}</span>
            </a>
          ))}
        </div>
      )}
    </section>
  );
}

const VAULT_TAGS: Record<number, string> = {
  16: "Vault created", 17: "Deposit requested", 18: "Withdrawal requested", 19: "Epoch settled", 20: "Claimed",
  21: "Matcher renewed", 22: "Profit converted", 23: "Market settled", 24: "Redeemed", 25: "Swept", 26: "Market listed", 27: "Fees harvested", 28: "Position unwound",
};

function describe(tx: any, v: C.Vault): string {
  if (!tx) return "Transaction";
  const msg = tx.transaction.message;
  const keys: PublicKey[] = (msg.staticAccountKeys ?? msg.accountKeys) as PublicKey[];
  const ixs: any[] = msg.compiledInstructions ?? msg.instructions;
  for (const ix of ixs) {
    const program = keys[ix.programIdIndex];
    const data: Uint8Array = ix.data instanceof Uint8Array ? ix.data : new Uint8Array(ix.data);
    if (program.equals(C.VAULT_PROGRAM) && VAULT_TAGS[data[0]]) {
      if (data[0] === 17) return `Deposit requested · ${usd(new DataView(data.buffer, data.byteOffset).getBigUint64(1, true))}`;
      return VAULT_TAGS[data[0]];
    }
    if (program.equals(C.PERCOLATOR) && data[0] === 10) {
      const dv = new DataView(data.buffer, data.byteOffset);
      const lo = dv.getBigUint64(51, true), hi = dv.getBigInt64(59, true); // size_q follows ids, epochs, sequence, asset and market id
      const size = (hi << 64n) + lo;
      return `${size > 0n ? "Long" : "Short"} ${units(size < 0n ? -size : size)} ${v.feed?.symbol ?? ""} vs vault`;
    }
    if (program.equals(C.ROUTER)) {
      if (data[0] === 4) {
        const size = new DataView(data.buffer, data.byteOffset).getBigInt64(1, true); // low 64 bits of the i128 size
        return `${size > 0n ? "Long" : "Short"} ${units(size < 0n ? -size : size)} ${v.feed?.symbol ?? ""} queued`;
      }
      if (data[0] === 5) return ""; // mark moved to a new Pyth price: not shown
      if (data[0] === 6) return "Queued trade filled at its target price";
      if (data[0] === 7) return "Queued trade expired unfilled";
    }
    if (program.equals(C.PERCOLATOR) && data[0] === 5) return "Keeper crank";
  }
  return "Transaction";
}

// ---------------------------------------------------------------- trade

function PendingTrade({ r, sym, chainNow }: { r: C.RouterRequest; sym: string; chainNow: number }) {
  const left = r.target - chainNow;
  return (
    <div className="callout small">
      <b>{r.size > 0n ? "Long" : "Short"} {units(r.size < 0n ? -r.size : r.size)} {sym} queued.</b>{" "}
      It fills at the first Pyth price published at or after {new Date(r.target * 1000).toLocaleTimeString()}
      {left > 0 ? ` (in ${left}s)` : ""}. Nobody can know that price yet, including you, so nobody can trade
      ahead of it. It cannot be cancelled; if no executor fills it within {C.ROUTER_GRACE_SECS}s it expires and
      nothing is traded.
    </div>
  );
}

function TradePanel({ state, send, busy }: { state: C.State; send: Send; busy?: string }) {
  const { publicKey } = useWallet();
  const [size, setSize] = useState("1");
  const [margin, setMargin] = useState("100");
  const v = state.vault, p = state.portfolio, sym = v.feed?.symbol ?? "";
  const notional = Number(size || 0) * num(state.asset.price, 1e6);
  const usdcAtoms = BigInt(Math.floor(Number(margin || 0) * C.USDC));
  const pendingHere = state.pending;
  const pendingElsewhere = !!state.trader?.hasPending && !pendingHere;
  const locked = !!state.trader?.hasPending;
  const chainNow = Math.floor(Date.now() / 1000);
  // Same rule as the vault's matcher: past the end of an epoch with deposits or withdrawals
  // waiting, the vault only takes trades that shrink its own position, so it can settle them.
  const reduceOnly = state.slot >= v.epochStart + v.epochLen && (v.pendingDeposit > 0n || v.pendingWithdraw > 0n);

  const create = () => {
    if (!publicKey) return;
    send("Create trading account", [C.openTradingAccount(publicKey)]);
  };
  const request = (q: bigint, label: string) => {
    if (!publicKey || !state.book) return;
    send(label, [C.requestTrade(publicKey, v.key, state.book.nextId, q)]);
  };
  const trade = (sign: 1n | -1n) => request(BigInt(Math.round(Number(size) * 1e6)) * sign, `Queue ${sign > 0n ? "long" : "short"} ${size} ${sym}`);
  const close = () => p && p.position !== 0n && request(-p.position, "Queue close");

  return (
    <>
      <section className="card">
        <h3>Your trading account</h3>
        {!publicKey ? <p className="muted">Connect a wallet to trade.</p> : !state.trader ? (
          <>
            <p className="muted small">
              Trading goes through the router, so your Percolator account is held by a program address derived from your
              wallet: only the router can trade it, and withdrawals can only go back to this wallet. One account covers
              every market (about 0.07 devnet SOL of rent).
            </p>
            <button className="btn primary wide" disabled={!!busy} onClick={create}>Create trading account</button>
            <Faucet />
          </>
        ) : (
          <>
            <div className="row"><span>Margin</span><b>{usd(p?.capital ?? 0n)}</b></div>
            <div className="row"><span>{sym} position</span><b className={(p?.position ?? 0n) > 0n ? "up" : (p?.position ?? 0n) < 0n ? "down" : ""}>{units(p?.position ?? 0n)} {sym}</b></div>
            <div className="row"><span>Position value</span><b>${fmt(Math.abs(num(p?.position ?? 0n, 1e6)) * num(state.asset.price, 1e6))}</b></div>
            <div className="row"><span>Wallet test USDC</span><b>{usd(state.userCollateral)}</b></div>
            <label className="field"><span>USDC</span><input value={margin} onChange={e => setMargin(e.target.value)} inputMode="decimal" /></label>
            <div className="btns">
              <button className="btn" disabled={!!busy || usdcAtoms <= 0n || usdcAtoms > state.userCollateral}
                onClick={() => send("Add margin", [createAssociatedTokenAccountIdempotentInstruction(publicKey, C.ata(publicKey, C.MINT), publicKey, C.MINT), C.routerDeposit(publicKey, usdcAtoms)])}>Add margin</button>
              <button className="btn" disabled={!!busy || locked || usdcAtoms <= 0n || usdcAtoms > (p?.capital ?? 0n)}
                onClick={() => send("Withdraw margin", [createAssociatedTokenAccountIdempotentInstruction(publicKey, C.ata(publicKey, C.MINT), publicKey, C.MINT), C.routerWithdraw(publicKey, usdcAtoms)])}>Withdraw</button>
            </div>
            {locked && <p className="muted small">Withdrawals are paused while a trade is queued.</p>}
            <Faucet />
          </>
        )}
      </section>
      <section className="card">
        <h3>Trade {sym}-PERP</h3>
        <label className="field"><span>Size ({sym})</span><input value={size} onChange={e => setSize(e.target.value)} inputMode="decimal" /></label>
        <div className="muted small">Notional ≈ ${fmt(notional)} · margin ≈ ${fmt(notional / 10)} · fee ≈ ${fmt(notional * 0.0005)}</div>
        <div className="btns">
          <button className="btn long" disabled={!state.trader || !state.book || locked || !!busy} onClick={() => trade(1n)}>Long</button>
          <button className="btn short" disabled={!state.trader || !state.book || locked || !!busy} onClick={() => trade(-1n)}>Short</button>
        </div>
        <button className="btn ghost wide" disabled={!p || p.position === 0n || locked || !!busy} onClick={close}>Close position</button>
        {reduceOnly && (
          <div className="note">
            This vault is settling an epoch: until it rolls (usually under a minute) it only fills trades that reduce
            its own position{v.inventory === 0n ? ", and it has none, so a trade queued now fills zero" : ` (it is ${v.inventory > 0n ? "long" : "short"}, so only ${v.inventory > 0n ? "longs" : "shorts"} fill)`}.
          </div>
        )}
        {pendingHere && <PendingTrade r={pendingHere} sym={sym} chainNow={chainNow} />}
        {pendingElsewhere && <p className="muted small">You have a trade queued on another market; it has to fill or expire first.</p>}
        <p className="muted small">
          Every trade is queued and fills at the first Pyth price published {C.ROUTER_DELAY_SECS} seconds after your
          request lands, so no one can trade against a price they already know. The vault may fill less than you ask
          when it nears its position limit or is settling an epoch.
        </p>
      </section>
    </>
  );
}

// ---------------------------------------------------------------- liquidity

function LiquidityPanel({ state, send, busy }: { state: C.State; send: Send; busy?: string }) {
  const { publicKey } = useWallet();
  const [amount, setAmount] = useState("100");
  const [out, setOut] = useState("");
  const v = state.vault, t = state.ticket;
  const nav = C.navOf(state);
  const price = state.shareSupply > 0n ? num(nav, C.USDC) / num(state.shareSupply, 10 ** C.SHARE_DECIMALS) : 1;
  const claimable = !!t && (t.deposit > 0n || t.withdraw > 0n) && t.epoch < v.epoch;
  const pending = !!t && (t.deposit > 0n || t.withdraw > 0n) && t.epoch === v.epoch;
  const atoms = BigInt(Math.floor(Number(amount || 0) * C.USDC));
  const tooMuch = atoms > state.userCollateral;
  const claimIxs = (u: PublicKey) => [
    createAssociatedTokenAccountIdempotentInstruction(u, C.ata(u, v.shareMint), u, v.shareMint),
    createAssociatedTokenAccountIdempotentInstruction(u, C.ata(u, C.MINT), u, C.MINT),
    C.claim(v, u, t!.epoch),
  ];
  return (
    <>
      <section className="card">
        <h3>Your liquidity</h3>
        {!publicKey ? <p className="muted">Connect a wallet to provide liquidity.</p> : (
          <>
            <div className="row"><span>Shares</span><b>{shares(state.userShares)}</b></div>
            <div className="row"><span>Value</span><b>${fmt(num(state.userShares, 10 ** C.SHARE_DECIMALS) * price)}</b></div>
            <div className="row"><span>Share price</span><b>${fmt(price, 4)}</b></div>
            <div className="row"><span>Wallet test USDC</span><b>{usd(state.userCollateral)}</b></div>
            {pending && <div className="note">Queued for epoch {String(t!.epoch)}: {t!.deposit > 0n && `${usd(t!.deposit)} deposit`} {t!.withdraw > 0n && `${shares(t!.withdraw)} shares out`}. It settles at one price when the epoch ends.</div>}
            {claimable && <button className="btn primary wide" disabled={!!busy} onClick={() => send("Claim", claimIxs(publicKey))}>Claim epoch {String(t!.epoch)} result</button>}
            <Faucet />
          </>
        )}
      </section>
      <section className="card">
        <h3>Deposit or withdraw</h3>
        <label className="field"><span>Deposit USDC</span><input value={amount} onChange={e => setAmount(e.target.value)} inputMode="decimal" /></label>
        <button className="btn primary wide" disabled={!publicKey || !!busy || v.status !== 1 || atoms <= 0n || tooMuch}
          onClick={() => publicKey && send("Deposit request", [
            ...(claimable ? claimIxs(publicKey) : []),
            createAssociatedTokenAccountIdempotentInstruction(publicKey, C.ata(publicKey, C.MINT), publicKey, C.MINT),
            C.requestDeposit(v, publicKey, atoms),
          ])}>Request deposit</button>
        {publicKey && tooMuch && <p className="small down">You have {usd(state.userCollateral)} test USDC. Use "Get test USDC" first.</p>}
        <label className="field"><span>Withdraw shares</span><input value={out} placeholder={shares(state.userShares)} onChange={e => setOut(e.target.value)} inputMode="decimal" /></label>
        <button className="btn wide" disabled={!publicKey || !!busy || state.userShares === 0n || v.status !== 1}
          onClick={() => publicKey && send("Withdraw request", [
            ...(claimable ? claimIxs(publicKey) : []),
            C.requestWithdraw(v, publicKey, out ? BigInt(Math.floor(Number(out) * 10 ** C.SHARE_DECIMALS)) : state.userShares),
          ])}>Request withdrawal</button>
        <p className="muted small">Requests settle together at one price when the epoch ends, so nobody trades against a stale share price. Withdrawals can't be blocked: if a trader holds a position to stop the vault going flat, anyone can make the vault close it one epoch later.</p>
      </section>
    </>
  );
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
    } catch (e: any) { setMsg(e.message); }
  };
  return (
    <div className="faucet">
      <button className="btn ghost wide" onClick={drip}>Get test USDC</button>
      {msg && <div className="muted small">{msg}</div>}
    </div>
  );
}

// ---------------------------------------------------------------- launch

function LaunchPage({ send, busy, go, initial }: { send: Send; busy?: string; go: (to: string) => void; initial?: string }) {
  const { connection } = useConnection();
  const { publicKey } = useWallet();
  const [prices, setPrices] = useState<Record<string, bigint>>({});
  const [status, setStatus] = useState<Record<string, number>>({}); // -1 none, 0 pending, 1 live
  const [feed, setFeed] = useState(C.FEEDS.find(f => f.symbol === initial) ?? C.FEEDS[0]);
  const [seedDeposit, setSeedDeposit] = useState("500");
  const [step, setStep] = useState(0);

  const load = useCallback(async () => {
    const keys = [...C.FEEDS.map(f => C.feedAccount(f.id)), ...C.FEEDS.map(f => C.canonicalVaultAddress(f.id))];
    const accs = await connection.getMultipleAccountsInfo(keys, "confirmed");
    const p: Record<string, bigint> = {}, st: Record<string, number> = {};
    C.FEEDS.forEach((f, i) => {
      const a = accs[i];
      if (a) p[f.symbol] = C.decodePyth(new Uint8Array(a.data)).e6;
      const v = accs[C.FEEDS.length + i];
      st[f.symbol] = v ? C.decodeVault(keys[C.FEEDS.length + i], new Uint8Array(v.data)).status : -1;
    });
    setPrices(p);
    setStatus(st);
  }, [connection]);
  useEffect(() => { load(); const id = setInterval(load, 10000); return () => clearInterval(id); }, [load]);

  const vault = C.canonicalVaultAddress(feed.id);
  const st = status[feed.symbol] ?? -1;

  const open = async () => {
    if (!publicKey) return;
    if (st === -1) {
      setStep(1);
      const m = await C.fetchMarket(connection);
      if (!(await send("Create vault", [C.initVault(publicKey, feed.id, m.nextMarketId).ix]))) return setStep(0);
    }
    setStep(2);
    // Listing appends a new asset slot, so read the market right before it.
    const m = await C.fetchMarket(connection);
    const pyth = C.decodePyth(new Uint8Array((await connection.getAccountInfo(C.feedAccount(feed.id)))!.data));
    const listed = await send(`List ${feed.symbol}-PERP`, [
      createAssociatedTokenAccountIdempotentInstruction(publicKey, C.ata(publicKey, C.MINT), publicKey, C.MINT),
      C.listAsset(publicKey, vault, feed.id, m.listSlot, m.nextMarketId, pyth.e6),
      C.openBook(publicKey, vault),
    ]);
    await load();
    if (!listed) return setStep(0);
    setStep(3);
    const dep = BigInt(Math.floor(Number(seedDeposit || 0) * C.USDC));
    if (dep > 0n) {
      const s = await C.fetchVault(connection, vault, publicKey);
      if (s.userCollateral >= dep) await send("Seed deposit", [C.requestDeposit(s.vault, publicKey, dep)]);
    }
    setStep(0);
    go(`/m/${vault.toString()}`);
  };

  return (
    <main className="grid">
      <section className="card span2">
        <h2>Open a market</h2>
        <p className="muted">
          Each Pyth feed gets exactly one market and one shared liquidity vault, enforced on-chain: the vault's address is derived
          from the feed, so a second one can't exist. Whoever opens it pays about 0.12 devnet SOL of rent and a 1 USDC listing fee,
          and chooses nothing else. Every market runs on the same fixed rules, so nobody can open a market set up to fail. In
          return, the opener earns 10% of the market's fees for as long as it trades.
        </p>
        <div className="feed-grid">
          {C.FEEDS.map(f => (
            <button key={f.symbol} className={f.symbol === feed.symbol ? "feed active" : "feed"} onClick={() => setFeed(f)}>
              <Icon symbol={f.symbol} />
              <div className="feed-sym">{f.symbol}</div>
              <div className="muted small">{prices[f.symbol] ? px(prices[f.symbol]) : "…"}</div>
              {status[f.symbol] === 1 ? <div className="tag">live</div> : status[f.symbol] === 0 ? <div className="tag warn-tag">unlisted</div> : null}
            </button>
          ))}
        </div>
      </section>
      <section className="card">
        <h3>The rules every market gets</h3>
        <div className="row"><span>Vault position limit</span><b>3× its NAV</b></div>
        <div className="row"><span>Largest single fill</span><b>0.75× its NAV</b></div>
        <div className="row"><span>Epoch</span><b>about {Math.round(C.CANON_EPOCH_LEN_SLOTS * C.slotSeconds() / 60)} minutes</b></div>
        <div className="row"><span>Insurance kept for traders</span><b>$100</b></div>
        <div className="row"><span>Price</span><b>Pyth, ≤ 5 min old</b></div>
        <div className="row"><span>Leverage, fee</span><b>10×, 0.05%</b></div>
        <div className="row"><span>Opener's share of fees</span><b>10%, forever</b></div>
        <p className="muted small">Limits scale with the vault: an empty vault quotes nothing, and every deposit deepens the market. Depositors share the other 90% of fees pro rata.</p>
      </section>
      <section className="card">
        <h3>{st === 1 ? `${feed.symbol}-PERP is live` : st === 0 ? `Finish listing ${feed.symbol}-PERP` : `Open ${feed.symbol}-PERP`}</h3>
        {st === 1 ? (
          <>
            <p className="muted">This market already exists. Trade it or add to its shared pool.</p>
            <button className="btn primary wide" onClick={() => go(`/m/${vault.toString()}`)}>Go to {feed.symbol}-PERP</button>
          </>
        ) : (
          <>
            <label className="field"><span>Your seed deposit (USDC, optional)</span><input value={seedDeposit} onChange={e => setSeedDeposit(e.target.value)} inputMode="decimal" /></label>
            <ol className="stepper">
              <li className={st === 0 || step > 1 ? "done" : step === 1 ? "active" : ""}>Create the vault {st === 0 && "(done)"}</li>
              <li className={step > 2 ? "done" : step === 2 ? "active" : ""}>List {feed.symbol}-PERP with its Pyth feed</li>
              <li className={step === 3 ? "active" : ""}>Seed deposit (settles when the first epoch ends)</li>
            </ol>
            <button className="btn primary wide" disabled={!publicKey || !!busy || !prices[feed.symbol]} onClick={open}>
              {!publicKey ? "Connect a wallet" : busy ? `${busy}…` : st === 0 ? `Finish listing ${feed.symbol}-PERP` : `Open ${feed.symbol}-PERP`}
            </button>
          </>
        )}
        <Faucet />
      </section>
    </main>
  );
}

// ---------------------------------------------------------------- leaderboard

type Row = { wallet: string; seed?: string; pnl: number; roi: number; volume: number; trades: number; equity: number; markets: string[]; positions: { symbol: string; size: number }[] };

function LeaderboardPage() {
  const { publicKey } = useWallet();
  const [data, setData] = useState<{ updated: number; rows: Row[] }>();
  const [err, setErr] = useState<string>();
  const [sort, setSort] = useState<"pnl" | "roi" | "volume">("pnl");
  useEffect(() => {
    const load = () => fetch("/api/leaderboard").then(r => r.json()).then(j => (j.error ? setErr(j.error) : (setData(j), setErr(undefined)))).catch(e => setErr(e.message));
    load();
    const id = setInterval(load, 30000);
    return () => clearInterval(id);
  }, []);
  const rows = [...(data?.rows ?? [])].sort((a, b) => b[sort] - a[sort]);
  const me = publicKey?.toString();
  const signed = (x: number, dp = 2) => `${x >= 0 ? "+" : "−"}$${fmt(Math.abs(x), dp)}`;
  return (
    <main className="grid">
      <section className="card span2">
        <div className="card-head">
          <div>
            <h2>Trader leaderboard</h2>
            <p className="muted small">Every trader on every vault market, ranked from on-chain data: PnL is account equity minus net deposits. Trade against any vault to get on the board. Wallets tagged "seed" are demo activity we created ourselves, labelled so they are never mistaken for users.</p>
          </div>
          <div className="seg narrow">
            {(["pnl", "roi", "volume"] as const).map(k => <button key={k} className={sort === k ? "seg-b active" : "seg-b"} onClick={() => setSort(k)}>{k === "pnl" ? "PnL" : k === "roi" ? "Return" : "Volume"}</button>)}
          </div>
        </div>
        {err && <div className="banner warn">{err}</div>}
        {!data ? <div className="loading">Reading the chain…</div> : rows.length === 0 ? <div className="muted">No trades yet. Be the first.</div> : (
          <div className="table-wrap">
            <table className="lb">
              <thead><tr><th>#</th><th>Trader</th><th className="r">PnL</th><th className="r">Return</th><th className="r">Volume</th><th className="r">Trades</th><th>Markets</th><th>Open</th></tr></thead>
              <tbody>
                {rows.map((r, i) => (
                  <tr key={r.wallet} className={r.wallet === me ? "me" : ""}>
                    <td className="rank">{i < 3 ? ["🥇", "🥈", "🥉"][i] : i + 1}</td>
                    <td><a href={explorer("address", r.wallet)} target="_blank" rel="noreferrer">{short(r.wallet)}</a>{r.wallet === me && <span className="you">you</span>}{r.seed && <span className="seed" title="Demo activity seeded by the team, labelled on purpose">{r.seed}</span>}</td>
                    <td className={`r mono ${r.pnl >= 0 ? "up" : "down"}`}>{signed(r.pnl)}</td>
                    <td className={`r mono ${r.roi >= 0 ? "up" : "down"}`}>{(r.roi * 100).toFixed(2)}%</td>
                    <td className="r mono">${fmt(r.volume, 0)}</td>
                    <td className="r mono">{r.trades}</td>
                    <td>{r.markets.join(", ") || "—"}</td>
                    <td className="small">{r.positions.length ? r.positions.map(p => `${p.size > 0 ? "+" : ""}${p.size} ${p.symbol}`).join(", ") : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {data && <p className="muted small">Updated {ago((Date.now() - data.updated) / 1000)}. Open positions count at their last settled value until they are closed.</p>}
      </section>
    </main>
  );
}
