// Router executor: fills queued trades at their target price and keeps every vault's mark fresh.
// Anyone can run one (the programs check everything); whoever fills a request earns its bond.
//
// For each live vault, every second and a half (without pause while a trade is queued):
//   1. Fill every request whose target the mark is at, once Percolator's price has reached it.
//      Percolator only takes new risk on an asset that is accrued to the current slot with every
//      position on it settled, so the fill carries those cranks (see `crankPlan` in chain.ts).
//      Their number depends on the slot the transaction lands in, so two versions are sent, one
//      crank apart: only the one that matches can succeed, and the request fills once.
//   2. Expire requests nobody filled within the grace period (their bond is forfeited).
//   3. Move the mark. Once a request's target time has passed, to the first Pyth price published
//      at or after it: fetched from Hermes (one request per target) and posted on chain.
//      Otherwise to Pyth's own sponsored feed account when it is newer (free: no Hermes call, no
//      posting), never past a pending target.
//   4. Every few minutes, refresh the shared feed accounts of feeds not listed yet, so opening a
//      market (which needs a price at most 300 s old) works. One batched Hermes request.
//
// Second price source: an exchange-median price service (JACK_URL). Its prices are unsigned, so
// they reach the chain through the router's reporter key (keys/reporter-keypair.json), which is
// trusted for them. With ORACLE=pyth (default) it is only the fallback: used for a request that
// Hermes has not served 20 s after its target, which is all the router accepts there. With
// ORACLE=jack (an X1 deployment, where there is no Pyth) it is the source, and the mark is also
// refreshed from it every 30 s or on a 0.3% move. The service has no history, so the executor
// records its stream itself to know the first price at or after a target.
//
// Hermes needs an API key (Pyth's free plan covers the feeds in chain.ts FEEDS). The key is read
// from HERMES_API_KEY or ~/.config/solana/percolator-test/hermes-key and never printed.
// Run: RPC=... npx tsx scripts/executor.ts
import { ComputeBudgetProgram, Connection, Keypair, PublicKey, SYSVAR_CLOCK_PUBKEY, Transaction, TransactionInstruction, TransactionMessage, VersionedTransaction } from "@solana/web3.js";
import * as fs from "fs";
import { createRequire } from "module";
import * as C from "../src/chain";

// Both SDKs are CommonJS; this package is an ES module.
const require = createRequire(import.meta.url);
const { Wallet } = require("@coral-xyz/anchor");
const { PythSolanaReceiver } = require("@pythnetwork/pyth-solana-receiver");

const conn = new Connection(process.env.RPC ?? C.RPC_URL, { commitment: "confirmed", disableRetryOnRateLimit: true, fetch: C.politeFetch(110) });
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(
  process.env.KEYPAIR ?? `${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
const HERMES = process.env.HERMES_URL ?? "https://pyth.dourolabs.app/hermes";
const API_KEY = process.env.HERMES_API_KEY ?? fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/hermes-key`, "utf8").trim();
const receiver = new PythSolanaReceiver({ connection: conn, wallet: new Wallet(payer) });
const PRIORITY = 1_000; // micro-lamports per CU: devnet needs little
const log = (...a: unknown[]) => console.log(new Date().toISOString().slice(11, 19), ...a);
// A rate-limited RPC call must not take the executor down: log it and carry on next pass.
process.on("unhandledRejection", e => log("rpc", String((e as any)?.message ?? e).split("\n")[0].slice(0, 120)));

// ---- the exchange-median price service, recorded locally ----
const ORACLE = process.env.ORACLE === "jack" ? "jack" : "pyth";
const JACK = process.env.JACK_URL ?? "http://jack0.x1.xyz:8090";
const JACK_SYMBOL: Record<string, string> = { XAU: "GOLD" };
const reporter = (() => {
  const p = process.env.REPORTER_KEY ?? "../keys/reporter-keypair.json";
  return fs.existsSync(p) ? Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf8")))) : null;
})();
type Sample = { t: number; price: number }; // t: unix seconds of the freshest quote used
const samples = new Map<string, Sample[]>();
let jackSymbols = "";

/** Keeps a ten-minute record of the service's stream for `symbols` (reconnects by itself). */
async function recordJack(symbols: string[]) {
  const want = [...new Set(symbols.map(s => JACK_SYMBOL[s] ?? s))].sort().join(",");
  if (!want || want === jackSymbols) return;
  jackSymbols = want;
  for (;;) {
    if (jackSymbols !== want) return; // the set of markets changed: a newer recorder took over
    try {
      const r = await fetch(`${JACK}/v1/stream/prices?symbols=${want.toLowerCase()}`);
      if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`);
      const reader = r.body.getReader();
      const dec = new TextDecoder();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done || jackSymbols !== want) break;
        buf += dec.decode(value, { stream: true });
        let i: number;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const line = buf.slice(0, i).split("\n").find(l => l.startsWith("data: "));
          buf = buf.slice(i + 2);
          if (!line) continue;
          for (const x of JSON.parse(line.slice(6)).data ?? []) {
            if (x.status !== "ok" || !(x.price > 0)) continue; // only prices enough exchanges agree on
            const list = samples.get(x.symbol) ?? samples.set(x.symbol, []).get(x.symbol)!;
            const t = Math.floor(new Date(x.updatedAt).getTime() / 1000);
            if (list.length && t < list[list.length - 1].t) continue;
            list.push({ t, price: x.price });
            while (list.length && list[0].t < t - 600) list.shift();
          }
        }
      }
    } catch (e: any) {
      log("price service stream:", String(e?.message ?? e).slice(0, 80));
    }
    await new Promise(r => setTimeout(r, 3000));
  }
}

/** The first recorded price with a timestamp at or after `t`, and the timestamp of the one
 *  before it (which is before `t`), or null if the record does not cover `t` yet. */
function jackFirstAtOrAfter(symbol: string, t: number): { price: bigint; publish: number; prev: number } | null {
  const list = samples.get(JACK_SYMBOL[symbol] ?? symbol) ?? [];
  const i = list.findIndex(x => x.t >= t);
  if (i <= 0) return null; // nothing at or after t yet, or no earlier sample to bound it
  return { price: BigInt(Math.round(list[i].price * 1e6)), publish: list[i].t, prev: list[i - 1].t };
}
function jackLatest(symbol: string) {
  const list = samples.get(JACK_SYMBOL[symbol] ?? symbol) ?? [];
  return list.length >= 2 ? { price: BigInt(Math.round(list[list.length - 1].price * 1e6)), publish: list[list.length - 1].t, prev: list[list.length - 2].t } : null;
}

/** Posts a reported price and moves the vault's mark to it, in one transaction. */
async function advanceReported(v: C.Vault, u: { price: bigint; publish: number; prev: number }, why: string) {
  if (!reporter) return false;
  // The previous timestamp must be before the new one, and the account only moves forward.
  const prev = Math.min(u.prev, u.publish - 1);
  const market = new Uint8Array((await conn.getAccountInfo(C.MARKET))!.data);
  const tx = new Transaction().add(...budget(), C.postPrice(reporter.publicKey, v.feed!.id, u.price, u.publish, prev), C.advanceMark(v, C.reportAddress(v.feed!.id), market));
  try {
    const sig = await conn.sendTransaction(tx, [payer, reporter]);
    await confirmed(sig);
    log("ok", `mark ${v.feed!.symbol} -> reported ${Number(u.price) / 1e6} at ${u.publish} (${why})`);
    return true;
  } catch (e: any) {
    const logs: string[] = e?.logs ?? [];
    log("fail", `mark ${v.feed!.symbol} -> reported ${u.publish} (${why})`, String(e?.message ?? e).split("\n")[0].slice(0, 100), logs.filter(l => /failed|error/.test(l)).slice(-1)[0] ?? "");
    return false;
  }
}

// ---- Hermes (rate-limited: back off for a minute on 429, as Hermes asks) ----
let hermesCalls = 0;
let backoffUntil = 0;
type Update = { vaa: string; publish: number; prev: number; feed: string };

async function hermes(path: string): Promise<any | null> {
  if (Date.now() < backoffUntil) return null;
  hermesCalls++;
  const r = await fetch(`${HERMES}${path}`, { headers: { Authorization: `Bearer ${API_KEY}` } });
  if (r.status === 429) {
    backoffUntil = Date.now() + 60_000;
    log("hermes rate limit: backing off 60 s");
    return null;
  }
  if (!r.ok) {
    log("hermes", r.status, path.split("?")[0]);
    return null;
  }
  return r.json();
}

const ids = (feeds: string[]) => feeds.map(f => `ids%5B%5D=${f}`).join("&");
const updates = (j: any): Update[] =>
  (j?.parsed ?? []).map((p: any, i: number) => ({ vaa: j.binary.data[Math.min(i, j.binary.data.length - 1)], publish: Number(p.price.publish_time), prev: Number(p.metadata.prev_publish_time), feed: p.id }));

/** The first Pyth update for `feed` published at or after `t` (prev_publish_time < t <= publish_time). */
async function firstAtOrAfter(feed: string, t: number): Promise<Update | null> {
  for (let s = t; s < t + 3; s++) {
    const u = updates(await hermes(`/v2/updates/price/${s}?${ids([feed])}&encoding=base64&parsed=true`))[0];
    if (!u) continue;
    if (u.prev < t && t <= u.publish) return u;
    if (u.publish >= t) return null;
  }
  return null;
}

// ---- chain ----
/** Waits for a signature by polling (the public RPC rate-limits the websockets confirmTransaction uses). */
async function confirmed(sig: string, processedIsEnough = false) {
  for (let i = 0; i < 50; i++) {
    await new Promise(r => setTimeout(r, 400));
    const st = (await conn.getSignatureStatuses([sig]).catch(() => null))?.value?.[0];
    if (st?.err) throw new Error(`transaction failed: ${JSON.stringify(st.err)}`);
    if (st?.confirmationStatus === "confirmed" || st?.confirmationStatus === "finalized" || (processedIsEnough && st?.confirmationStatus === "processed")) return;
  }
  throw new Error("not confirmed within 20 s");
}

/** Sends (with preflight). Only fills wait for confirmation; routine work is checked again on the
 *  next pass from chain state, so it must never hold up a trade's fill. */
async function send(label: string, ixs: TransactionInstruction[], wait = false) {
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.setComputeUnitPrice({ microLamports: PRIORITY }), ...ixs);
  try {
    const sig = await conn.sendTransaction(tx, [payer]);
    if (wait) await confirmed(sig);
    log(wait ? "ok" : "sent", label);
    return true;
  } catch (e: any) {
    const logs: string[] = e?.logs ?? [];
    // The failing instruction is the last top-level one that started (two budget instructions come first).
    const at = logs.filter(l => l.endsWith("invoke [1]")).length - 3;
    const ix = ixs[at];
    const what = ix?.data[0] === 5 ? `crank #${at} asset ${ix.data[10]} via ${ix.keys[2].pubkey.toBase58().slice(0, 6)}` : ix ? `instruction #${at}` : "";
    log("fail", label, String(e?.message ?? e).split("\n")[0].slice(0, 120), logs.filter(l => /failed|error/.test(l)).slice(-1)[0] ?? "", what);
    return false;
  }
}

/** Sends the Pyth builder's transactions in order (post, consume, close), confirming each by polling. */
async function sendBuilt(txs: { tx: any; signers: any[] }[]) {
  // Each transaction builds on the one before, so they go in order; only the last has to be
  // confirmed before acting on it, the others just have to have been processed.
  for (const [i, { tx, signers }] of txs.entries()) {
    tx.sign([payer, ...signers]);
    const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true });
    await confirmed(sig, i < txs.length - 1);
  }
}

/** Posts one verified update (ephemeral account) and moves the vault's mark to it. */
async function advancePosted(v: C.Vault, u: Update) {
  const market = new Uint8Array((await conn.getAccountInfo(C.MARKET))!.data);
  const builder = receiver.newTransactionBuilder({ closeUpdateAccounts: true });
  await builder.addPostPriceUpdates([u.vaa]);
  await builder.addPriceConsumerInstructions(async (getAccount: (id: string) => any) => [
    { instruction: C.advanceMark(v, getAccount("0x" + v.feed!.id), market), signers: [] },
  ]);
  try {
    const txs = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: PRIORITY });
    const t0 = Date.now();
    await sendBuilt(txs);
    log("ok", `mark ${v.feed!.symbol} -> Pyth ${u.publish} (target price; ${txs.length} transactions, ${((Date.now() - t0) / 1000).toFixed(1)} s, started ${(t0 / 1000 - u.publish).toFixed(1)} s after the price was published)`);
  } catch (e: any) {
    log("fail", `mark ${v.feed!.symbol} -> ${u.publish}`, String(e?.message ?? e).slice(0, 140));
  }
}

async function requestsOf(v: C.Vault, b: C.Book) {
  const keys = b.pending.map(p => C.requestAddress(v.key, p.id));
  const accs = keys.length ? await conn.getMultipleAccountsInfo(keys) : [];
  return accs.flatMap((a, i) => (a ? [C.decodeRequest(keys[i], new Uint8Array(a.data))] : []));
}

type Snapshot = { book: Uint8Array | null; feed: Uint8Array | null; market: Uint8Array; slot: bigint };

const prepared = new Set<string>(); // requests whose assets were brought current while they waited
const LAND_SLOTS = 6; // slots between reading the chain and a transaction landing, roughly
const budget = () => [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.setComputeUnitPrice({ microLamports: PRIORITY })];

/** Simulates `k` lag cranks, the settlement cranks and `tail`, and adjusts to what the chain
 *  reports: a crank with nothing to do is dropped, and a trade refused because an asset is still
 *  behind gets one more lag crank. Returns what would succeed now, or why nothing does. */
async function tune(lagCrank: TransactionInstruction, k: number, settle: TransactionInstruction[], tail: TransactionInstruction[]) {
  const seen: string[] = [];
  for (let round = 0; round < 10; round++) {
    // The simulation replaces the blockhash, so any well-formed one will do.
    const msg = new TransactionMessage({ payerKey: payer.publicKey, recentBlockhash: PublicKey.default.toBase58(), instructions: [...budget(), ...Array(k).fill(lagCrank), ...settle, ...tail] }).compileToV0Message();
    const r = await conn.simulateTransaction(new VersionedTransaction(msg), { sigVerify: false, replaceRecentBlockhash: true });
    const err: any = r.value.err;
    if (!err) return { k, settle };
    const i = (err.InstructionError?.[0] ?? -1) - 2, code = err.InstructionError?.[1]?.Custom;
    seen.push(`${code ?? JSON.stringify(err.InstructionError?.[1])} at ${i} of ${k}+${settle.length}+${tail.length}`);
    if (code === 22 && i >= 0 && i < k) k--;
    else if (code === 22 && i >= k && i < k + settle.length) settle = settle.filter((_, j) => j !== i - k);
    else if (code === 21 && i >= k && k < 12) k++;
    else return `simulation failed at instruction ${i} of ${k} + ${settle.length} + ${tail.length}: ${JSON.stringify(err)}`;
  }
  return `simulation kept changing: ${seen.join("; ")}`;
}

/** Sends alternative versions of one action at once (no preflight) and waits until one lands. */
async function race(label: string, variants: TransactionInstruction[][]) {
  const sigs: string[] = [];
  for (const ixs of variants) {
    const sig = await conn.sendTransaction(new Transaction().add(...budget(), ...ixs), [payer], { skipPreflight: true }).catch(() => null);
    if (sig) sigs.push(sig);
  }
  for (let i = 0; i < 40 && sigs.length; i++) {
    await new Promise(r => setTimeout(r, 500));
    const st = (await conn.getSignatureStatuses(sigs).catch(() => null))?.value ?? [];
    if (st.some(x => x && !x.err && (x.confirmationStatus === "confirmed" || x.confirmationStatus === "finalized"))) { log("ok", label); return true; }
    if (st.length === sigs.length && st.every(x => x?.err)) { log("fail", label, st.map(x => JSON.stringify(x!.err)).join(" / ")); return false; }
  }
  log("fail", label, "not confirmed within 20 s");
  return false;
}

/** Forced closes and bookkeeping that nobody else does, while no trade is queued:
 *   - Close-outs. A close-only market (a position on it was force-reduced: the vault's Unwind, or
 *     Percolator's own liquidation) reopens only once every position on it is closed, and the
 *     vault cannot take the other side of a close there.
 *   - Liquidations. An account below the router's level is closed against the vault, before it
 *     reaches Percolator's maintenance margin (whose liquidation makes the market close-only).
 *   - A vault's tracked position is re-read when Percolator changed it outside a fill.
 *  Anyone may queue a forced close for a trader; this queues one per pass and market, and the
 *  fill path executes it at its target Pyth price like any request. */
async function watch(vaults: C.Vault[], market: Uint8Array, full: boolean) {
  const closing = vaults.filter(v => C.closeOnly(market, v.assetIndex));
  if (!closing.length && !full) return false;
  const [snap, traders, fresh] = await Promise.all([
    C.marketSnapshot(conn),
    conn.getProgramAccounts(C.ROUTER, { filters: [{ dataSize: C.TRADER_LEN }] }),
    conn.getMultipleAccountsInfo(vaults.map(v => v.key)),
  ]);
  const owners = new Map(traders.map(t => { const d = C.decodeTrader(new Uint8Array(t.account.data)); return [d.portfolio.toBase58(), d] as const; }));
  for (const [i, v] of vaults.entries()) {
    const lp = snap.portfolios.find(p => p.pubkey.equals(v.lpPortfolio));
    if (!lp || !fresh[i]) continue;
    const held = C.decodePortfolio(lp.data, v.assetIndex, snap.market).position;
    const tracked = C.decodeVault(v.key, new Uint8Array(fresh[i]!.data)).inventory;
    if (held !== tracked) await send(`sync ${v.feed?.symbol} vault position ${Number(tracked) / 1e6} -> ${Number(held) / 1e6}`, [C.syncInventory(v)]);
  }
  // An epoch that is over with deposits or withdrawals waiting, while the vault holds a position
  // (a flat vault is rolled directly by the keeper): settle it at a committed price if its
  // withdrawals fit in the vault's cash, otherwise it has to get flat first.
  let sent = false;
  for (const [i, v] of vaults.entries()) {
    const lp = snap.portfolios.find(p => p.pubkey.equals(v.lpPortfolio));
    if (!lp || !fresh[i]) continue;
    const cur = C.decodeVault(v.key, new Uint8Array(fresh[i]!.data));
    const over = BigInt(snap.slot) >= cur.epochStart + cur.epochLen;
    if (!over || cur.needsFlat || (cur.pendingDeposit === 0n && cur.pendingWithdraw === 0n)) continue;
    if (C.decodePortfolio(lp.data, v.assetIndex, snap.market).position === 0n || C.closeOnly(snap.market, v.assetIndex)) continue;
    const s = await C.fetchVault(conn, v.key);
    if (!s.book) continue;
    if (C.canSettleOpen(s)) sent = (await send(`queue settlement of ${v.feed?.symbol} epoch ${cur.epoch}`, [C.requestSettle(payer.publicKey, v.key, s.book.nextId)], true)) || sent;
    else await send(`require flat ${v.feed?.symbol} (withdrawals exceed the vault's cash)`, [C.requireFlat(v)]);
  }
  const queue = async (v: C.Vault, wallet: PublicKey, why: string) => {
    const book = await conn.getAccountInfo(C.bookAddress(v.key));
    if (!book) return false;
    return send(`queue ${why} ${v.feed?.symbol} for ${wallet.toBase58().slice(0, 6)}`, [C.requestClose(payer.publicKey, wallet, v.key, C.decodeBook(new Uint8Array(book.data)).nextId)], true);
  };
  const busy = new Set<string>();
  for (const v of closing) {
    const holder = snap.portfolios.find(p => { const o = owners.get(p.pubkey.toBase58()); return o && !o.hasPending && C.decodePortfolio(p.data, v.assetIndex, snap.market).position !== 0n; });
    if (!holder) continue;
    const t = owners.get(holder.pubkey.toBase58())!;
    busy.add(holder.pubkey.toBase58());
    sent = (await queue(v, t.wallet, "close-out")) || sent;
  }
  for (const p of snap.portfolios) {
    const o = owners.get(p.pubkey.toBase58());
    if (!o || o.hasPending || busy.has(p.pubkey.toBase58())) continue;
    const h = C.accountHealth(snap.market, p.data);
    if (!h.liquidatable) continue;
    // Close the largest position first; the next pass looks again.
    const worst = h.positions.reduce((a, b) => (b.notional > a.notional ? b : a));
    const v = vaults.find(x => x.assetIndex === worst.asset);
    if (v && !C.closeOnly(snap.market, v.assetIndex)) sent = (await queue(v, o.wallet, `liquidation (equity ${(Number(h.equity) / 1e6).toFixed(2)} on ${(Number(h.notional) / 1e6).toFixed(2)} notional)`)) || sent;
  }
  return sent;
}

/** Routine upkeep while no trade is queued: finalize parked sides, keep every listed asset within
 *  ~50 slots of the chain (one crank advances all of them by 10), and every third pass settle
 *  the positions that price moves left out of date, so a fill has little left to do. */
async function upkeep(vaults: C.Vault[], market: Uint8Array, slot: bigint, pass: number) {
  const assets = vaults.map(v => v.assetIndex);
  const resets = assets.flatMap(a => C.finalizeResets(market, a));
  if (resets.length) await send("finalize side reset", resets);
  const oldest = assets.map(a => C.decodeAsset(market, a).slotLast).reduce((m, t) => (t < m ? t : m), slot);
  const lag = Number(slot - oldest);
  const vehicle = vaults[0].lpPortfolio;
  if (pass % 3 === 0 && lag <= 60) {
    const plan = C.crankPlan(await C.marketSnapshot(conn), payer.publicKey, vehicle, undefined, undefined, assets);
    if (plan.settle.length) {
      const t = await tune(plan.lagCrank, Math.max(1, Math.ceil((plan.lag + LAND_SLOTS) / 10)), plan.settle.slice(0, 6), []);
      if (typeof t === "string") log("fail", "settle positions", t);
      else if (t.settle.length) await send(`settle ${t.settle.length} out-of-date portfolio(s)`, [...Array(t.k).fill(plan.lagCrank), ...t.settle]);
      return;
    }
  }
  if (lag > 50) await send(`crank ${assets.length} asset(s) (${lag} slots behind)`, Array(Math.min(10, Math.ceil(lag / 10))).fill(C.crank(payer.publicKey, vehicle, assets)));
}

/** Executes a queued epoch settlement: the vault's own position has to be settled at the mark
 *  (one crank on its portfolio once its asset is current), then the router rolls the epoch. */
async function settleEpoch(v: C.Vault, r: C.RouterRequest, what: string) {
  const [snap, va] = await Promise.all([C.marketSnapshot(conn), conn.getAccountInfo(v.key)]);
  if (!va) return;
  const cur = C.decodeVault(v.key, new Uint8Array(va.data));
  const own = { ...snap, portfolios: snap.portfolios.filter(p => p.pubkey.equals(v.lpPortfolio)) };
  const plan = C.crankPlan(own, payer.publicKey, v.lpPortfolio, v.assetIndex);
  const k = Math.max(1, Math.ceil((plan.lag + LAND_SLOTS) / 10));
  if (k > 8) {
    await send(`catch up for settlement ${what} (${plan.lag} slots behind)`, Array(Math.min(k, 12)).fill(plan.lagCrank), true);
    return;
  }
  const tail = [C.settleEpoch(payer.publicKey, cur, r, C.marketHeader(snap.market).nextMarketId)];
  const t = await tune(plan.lagCrank, k, plan.settle, tail);
  if (typeof t === "string") {
    log("fail", `settle epoch ${cur.epoch} of ${what}`, t);
    // 0x560e: the withdrawals do not fit in the vault's cash after all. It has to get flat.
    if (t.includes('"Custom":22030') && !cur.needsFlat) await send(`require flat ${v.feed?.symbol}`, [C.requireFlat(v)]);
    return;
  }
  const version = (n: number) => [...Array(n).fill(plan.lagCrank), ...t.settle, ...tail];
  await race(`settle epoch ${cur.epoch} of ${what} at ${Number(C.decodeAsset(snap.market, v.assetIndex).price) / 1e6} with the vault's position open`, [version(t.k), version(t.k + 1)]);
}

async function serviceVault(v: C.Vault, now: number, snap: Snapshot, queued: boolean): Promise<boolean> {
  if (!snap.book) {
    await send(`open book ${v.feed?.symbol}`, [C.openBook(payer.publicKey, v.key)]);
    return false;
  }
  let b = C.decodeBook(snap.book);
  const reqs = await requestsOf(v, b);
  const live = reqs.filter(r => now <= r.target + C.ROUTER_GRACE_SECS);

  // 0. While a request waits for its target time, bring every asset its fill will touch up to
  //    the chain (once per request), so the fill itself has only a few seconds to catch up.
  for (const r of live.filter(r => r.target > b.markPublish && now < r.target && !prepared.has(`${v.key}:${r.id}`))) {
    prepared.add(`${v.key}:${r.id}`);
    const plan = C.crankPlan(await C.marketSnapshot(conn), payer.publicKey, v.lpPortfolio, v.assetIndex, r.settle ? undefined : r.wallet);
    if (plan.lag <= 15) continue;
    const t = await tune(plan.lagCrank, Math.min(10, Math.ceil(plan.lag / 10)), [], []);
    if (typeof t !== "string" && t.k > 0) await send(`get ready for ${v.feed?.symbol} #${r.id} (${plan.lag} slots behind)`, Array(t.k).fill(plan.lagCrank));
  }

  // 1. Time-critical: once the earliest pending target has passed, post the first Pyth price at
  //    or after it (only while its requests can still fill), then fill at it right away.
  const waiting = live.filter(r => r.target > b.markPublish).map(r => r.target);
  const earliest = waiting.length ? Math.min(...waiting) : null;
  // (Wall time decides when to ask Hermes: the chain clock can run a second or two behind it.)
  if (earliest !== null && Math.max(now, Date.now() / 1000) >= earliest + 0.6 && now < earliest + C.ROUTER_GRACE_SECS - 3) {
    const u = ORACLE === "pyth" ? await firstAtOrAfter(v.feed!.id, earliest).catch(() => null) : null;
    let moved = false;
    if (u && u.publish > b.markPublish) {
      await advancePosted(v, u);
      moved = true;
    } else if (ORACLE === "jack" || Date.now() / 1000 >= earliest + C.REPORT_FALLBACK_SECS + 1) {
      // No Pyth price for this target (Hermes down, out of quota, or no Pyth on this chain):
      // the recorded exchange-median price at the target, through the reporter key.
      const j = jackFirstAtOrAfter(v.feed!.symbol, earliest);
      if (j && j.publish > b.markPublish) moved = await advanceReported(v, j, ORACLE === "jack" ? "target price" : "fallback: no Pyth price for this target");
    }
    if (moved) {
      const ba = await conn.getAccountInfo(C.bookAddress(v.key));
      if (ba) b = C.decodeBook(new Uint8Array(ba.data));
    }
  }

  // 2. Fill requests the mark is at. The cranks in front bring every asset involved to the
  //    current slot (which also walks Percolator's price to the mark) and settle the positions
  //    that leaves out of date; a miss is retried next pass.
  for (const r of live.filter(r => b.markPrev < r.target && r.target <= b.markPublish)) {
    const what = `${v.feed?.symbol} #${r.id}`;
    if (r.settle) { await settleEpoch(v, r, what); continue; }
    const plan = C.crankPlan(await C.marketSnapshot(conn), payer.publicKey, v.lpPortfolio, v.assetIndex, r.wallet);
    const k = Math.max(1, Math.ceil((plan.lag + LAND_SLOTS) / 10));
    if (k > 6) {
      // Too far behind to catch up inside the fill: advance first, wait, fill next pass.
      const t = await tune(plan.lagCrank, Math.min(k, 10), [], []);
      if (typeof t === "string") log("fail", `catch up for ${what}`, t);
      else await send(`catch up for ${what} (${plan.lag} slots behind)`, Array(t.k).fill(plan.lagCrank), true);
      continue;
    }
    const tail = [...plan.resets, C.fillRequest(payer.publicKey, v, r)];
    let t = await tune(plan.lagCrank, k, plan.settle, plan.settle.length > 3 ? [] : tail);
    if (typeof t !== "string" && plan.settle.length > 3 && t.settle.length <= 3) t = await tune(plan.lagCrank, t.k, t.settle, tail);
    if (typeof t === "string" ? t.includes("ProgramFailedToComplete") : t.settle.length > 3) {
      // Settling this many accounts does not fit in one transaction with the fill (compute
      // budget): settle first, wait, fill next pass. The price does not move again meanwhile.
      const first = typeof t === "string" ? await tune(plan.lagCrank, k, plan.settle.slice(0, 6), []) : { k: t.k, settle: t.settle.slice(0, 6) };
      if (typeof first === "string") log("fail", `settle for ${what}`, first);
      else await send(`settle ${first.settle.length} account(s) for ${what}`, [...Array(first.k).fill(plan.lagCrank), ...first.settle], true);
      continue;
    }
    if (typeof t === "string") { log("fail", `fill ${what}`, t); continue; }
    const tuned = t;
    // The lag crank count is right for the slot simulated; one more is right from the next
    // 10-slot boundary on. Whichever matches the landing slot fills; the other fails harmlessly.
    const version = (n: number) => [...Array(n).fill(plan.lagCrank), ...tuned.settle, ...tail];
    await race(`${r.size === 0n ? "forced close" : "fill"} ${what} size ${Number(r.size) / 1e6} at ${Number(b.markPrice) / 1e6} (${tuned.k} lag + ${tuned.settle.length} settle cranks)`, [version(tuned.k), version(tuned.k + 1)]);
    log(`  ${what}: ${Math.round(Date.now() / 1000 - (r.target - C.ROUTER_DELAY_SECS))} s from request to result`);
  }

  // 3. Expire requests past their grace period.
  for (const r of reqs.filter(r => now > r.target + C.ROUTER_GRACE_SECS)) {
    await send(`expire ${v.feed?.symbol} #${r.id}`, [C.expireRequest(payer.publicKey, r)]);
  }

  // 4. Free: move the mark to Pyth's sponsored feed account when it is newer, never past a
  //    pending target.
  if (ORACLE === "jack" && live.length === 0 && !queued) {
    // No sponsored feed accounts here: refresh the mark from the price service every 30 s or
    // on a 0.3% move (each refresh is a transaction, so not on every tick).
    const j = jackLatest(v.feed!.symbol);
    const moved = j && b.markPrice > 0n ? Math.abs(Number(j.price) / Number(b.markPrice) - 1) : 1;
    if (j && j.publish > b.markPublish && (j.publish - b.markPublish >= 30 || moved >= 0.003)) await advanceReported(v, j, "refresh");
  } else if (snap.feed && live.length === 0 && !queued) { // not while a trade waits on any market: it comes first
    const p = C.decodePyth(snap.feed);
    if (p.publish > b.markPublish) await send(`mark ${v.feed!.symbol} -> sponsored ${p.publish}`, [C.advanceMark(v, C.feedAccount(v.feed!.id), snap.market)]);
  }
  return reqs.length > 0;
}

let vaultList: C.Vault[] = [];
let lastFeedRefresh = 0;
let refreshing = false;
/** Keeps the shared (shard 0) feed accounts of unlisted feeds under ~4 minutes old, so opening a
 *  market works. One batched Hermes request for all stale ones, every 3 minutes at most. */
async function refreshUnlistedFeeds(listed: Set<string>) {
  const wall = Math.floor(Date.now() / 1000);
  if (wall - lastFeedRefresh < 180) return;
  lastFeedRefresh = wall;
  const stale: string[] = [];
  const accs = await conn.getMultipleAccountsInfo(C.FEEDS.map(f => C.feedAccount(f.id)));
  C.FEEDS.forEach((f, i) => {
    if (listed.has(f.id)) return;
    const a = accs[i];
    if (!a || wall - C.decodePyth(new Uint8Array(a.data)).publish > 200) stale.push(f.id);
  });
  if (!stale.length) return;
  const j = await hermes(`/v2/updates/price/latest?${ids(stale)}&encoding=base64`);
  if (!j) return;
  try {
    const builder = receiver.newTransactionBuilder({ closeUpdateAccounts: true });
    await builder.addUpdatePriceFeed(j.binary.data, 0);
    await sendBuilt(await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: PRIORITY }));
    log("ok", `refreshed ${stale.length} unlisted feed account(s)`);
  } catch (e: any) {
    log("fail", "refresh feeds", String(e?.message ?? e).slice(0, 140));
  }
}

(async () => {
  log("executor", payer.publicKey.toBase58(), "router", C.ROUTER.toBase58(), "prices:", ORACLE === "jack" ? `${JACK} (reported)` : `Pyth via ${HERMES}${reporter ? `, fallback ${JACK} (reported)` : ""}`);
  let lastStats = Date.now();
  let vaults: C.Vault[] = [];
  let vaultsAt = 0;
  let pass = 0;
  for (;;) {
    let waiting = false; // a trade is queued: come back sooner
    try {
      // The vault list is re-read every 20 seconds, so a market opened just now is serviced
      // before its first request can expire; everything else is one batched read per pass (the
      // public devnet RPC allows about 100 requests per 10 seconds).
      if (!vaults.length || Date.now() - vaultsAt > 20_000) {
        vaultList = await C.listVaults(conn);
        vaults = vaultList.filter(v => v.market.equals(C.MARKET) && v.canonical && v.status === 1 && v.feed);
        vaultsAt = Date.now();
        if (reporter) recordJack(vaults.map(v => v.feed!.symbol)); // runs in the background
      }
      const keys = [SYSVAR_CLOCK_PUBKEY, C.MARKET, ...vaults.flatMap(v => [C.bookAddress(v.key), C.feedAccount(v.feed!.id)])];
      const accs = await conn.getMultipleAccountsInfo(keys);
      const clock = new DataView(accs[0]!.data.buffer, accs[0]!.data.byteOffset);
      const now = Number(clock.getBigInt64(32, true));
      const slot = clock.getBigUint64(0, true);
      const market = new Uint8Array(accs[1]!.data);
      let busy = false;
      // While any trade is queued, routine cranking stops: the fill carries the exact cranks it
      // needs, and one landing in between would make one of those a no-op and fail the fill.
      // The fill catches up whatever the request needs itself.
      const queued = vaults.some((_, i) => accs[2 + 2 * i] && C.decodeBook(new Uint8Array(accs[2 + 2 * i]!.data)).pending.length > 0);
      if (!queued && vaults.length && (await watch(vaults, market, pass % 3 === 1).catch(e => { log("error", "watch", String(e?.message ?? e).slice(0, 140)); return false; }))) continue;
      if (!queued && vaults.length) await upkeep(vaults, market, slot, pass++).catch(e => log("error", "upkeep", String(e?.message ?? e).slice(0, 140)));
      waiting = queued;
      for (const [i, v] of vaults.entries()) {
        const snap = { market, slot, book: accs[2 + 2 * i] ? new Uint8Array(accs[2 + 2 * i]!.data) : null, feed: accs[3 + 2 * i] ? new Uint8Array(accs[3 + 2 * i]!.data) : null };
        if (queued && !(snap.book && C.decodeBook(snap.book).pending.length)) continue; // trades first
        busy = (await serviceVault(v, now, snap, queued).catch(e => { log("error", v.feed?.symbol, String(e?.message ?? e).slice(0, 140)); return false; })) || busy;
      }
      // Low priority, in the background, and only while no trade is waiting.
      if (!busy && !refreshing) {
        refreshing = true;
        refreshUnlistedFeeds(new Set(vaults.map(v => v.feed!.id))).finally(() => (refreshing = false));
      }
      if (Date.now() - lastStats > 600_000) {
        log(`hermes requests in the last 10 min: ${hermesCalls}`);
        hermesCalls = 0;
        lastStats = Date.now();
      }
    } catch (e: any) {
      log("error", String(e?.message ?? e).slice(0, 140));
    }
    await new Promise(r => setTimeout(r, waiting ? 500 : 1500));
  }
})();
