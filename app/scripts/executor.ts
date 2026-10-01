// Router executor: fills queued trades at their target price and keeps every vault's mark fresh.
// Anyone can run one (the programs check everything); whoever fills a request earns its bond.
//
// For each live vault, every three seconds (every second while a trade is queued):
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
// Hermes needs an API key (Pyth's free plan covers the feeds in chain.ts FEEDS). The key is read
// from HERMES_API_KEY or ~/.config/solana/percolator-test/hermes-key and never printed.
// Run: RPC=... npx tsx scripts/executor.ts
import { ComputeBudgetProgram, Connection, Keypair, SYSVAR_CLOCK_PUBKEY, Transaction, TransactionInstruction, TransactionMessage, VersionedTransaction } from "@solana/web3.js";
import * as fs from "fs";
import { createRequire } from "module";
import * as C from "../src/chain";

// Both SDKs are CommonJS; this package is an ES module.
const require = createRequire(import.meta.url);
const { Wallet } = require("@coral-xyz/anchor");
const { PythSolanaReceiver } = require("@pythnetwork/pyth-solana-receiver");

const conn = new Connection(process.env.RPC ?? C.RPC_URL, { commitment: "confirmed", disableRetryOnRateLimit: true, fetch: C.politeFetch() });
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(
  process.env.KEYPAIR ?? `${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
const HERMES = process.env.HERMES_URL ?? "https://pyth.dourolabs.app/hermes";
const API_KEY = process.env.HERMES_API_KEY ?? fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/hermes-key`, "utf8").trim();
const receiver = new PythSolanaReceiver({ connection: conn, wallet: new Wallet(payer) });
const PRIORITY = 1_000; // micro-lamports per CU: devnet needs little
const log = (...a: unknown[]) => console.log(new Date().toISOString().slice(11, 19), ...a);
// A rate-limited RPC call must not take the executor down: log it and carry on next pass.
process.on("unhandledRejection", e => log("rpc", String((e as any)?.message ?? e).split("\n")[0].slice(0, 120)));

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
async function confirmed(sig: string) {
  for (let i = 0; i < 20; i++) {
    await new Promise(r => setTimeout(r, 1000));
    const st = (await conn.getSignatureStatuses([sig]).catch(() => null))?.value?.[0];
    if (st?.err) throw new Error(`transaction failed: ${JSON.stringify(st.err)}`);
    if (st?.confirmationStatus === "confirmed" || st?.confirmationStatus === "finalized") return;
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
  for (const { tx, signers } of txs) {
    tx.sign([payer, ...signers]);
    const sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true });
    await confirmed(sig);
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
    await sendBuilt(await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: PRIORITY }));
    log("ok", `mark ${v.feed!.symbol} -> Pyth ${u.publish} (target price)`);
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

const LAND_SLOTS = 6; // slots between reading the chain and a transaction landing, roughly
const budget = () => [ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.setComputeUnitPrice({ microLamports: PRIORITY })];

/** Simulates `k` lag cranks, the settlement cranks and `tail`, and adjusts to what the chain
 *  reports: a crank with nothing to do is dropped, and a trade refused because an asset is still
 *  behind gets one more lag crank. Returns what would succeed now, or why nothing does. */
async function tune(lagCrank: TransactionInstruction, k: number, settle: TransactionInstruction[], tail: TransactionInstruction[]) {
  const seen: string[] = [];
  for (let round = 0; round < 10; round++) {
    const { blockhash } = await conn.getLatestBlockhash();
    const msg = new TransactionMessage({ payerKey: payer.publicKey, recentBlockhash: blockhash, instructions: [...budget(), ...Array(k).fill(lagCrank), ...settle, ...tail] }).compileToV0Message();
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
  for (let i = 0; i < 20 && sigs.length; i++) {
    await new Promise(r => setTimeout(r, 1000));
    const st = (await conn.getSignatureStatuses(sigs).catch(() => null))?.value ?? [];
    if (st.some(x => x && !x.err && (x.confirmationStatus === "confirmed" || x.confirmationStatus === "finalized"))) { log("ok", label); return true; }
    if (st.length === sigs.length && st.every(x => x?.err)) { log("fail", label, st.map(x => JSON.stringify(x!.err)).join(" / ")); return false; }
  }
  log("fail", label, "not confirmed within 20 s");
  return false;
}

/** A close-only market (a position on it was force-reduced, typically by the vault's Unwind)
 *  reopens only once every position on it is closed, and the vault cannot take the other side
 *  of a close there. Anyone may queue a close-out for a trader; this queues one per pass and
 *  market, and the fill path executes it at its target Pyth price like any request. */
async function closeOut(vaults: C.Vault[], market: Uint8Array) {
  const closing = vaults.filter(v => C.closeOnly(market, v.assetIndex));
  if (!closing.length) return false;
  const [snap, traders] = await Promise.all([C.marketSnapshot(conn), conn.getProgramAccounts(C.ROUTER, { filters: [{ dataSize: C.TRADER_LEN }] })]);
  const owners = new Map(traders.map(t => { const d = C.decodeTrader(new Uint8Array(t.account.data)); return [d.portfolio.toBase58(), d] as const; }));
  let sent = false;
  for (const v of closing) {
    const holder = snap.portfolios.find(p => owners.get(p.pubkey.toBase58()) && !owners.get(p.pubkey.toBase58())!.hasPending && C.decodePortfolio(p.data, v.assetIndex, snap.market).position !== 0n);
    const book = await conn.getAccountInfo(C.bookAddress(v.key));
    if (!holder || !book) continue;
    const t = owners.get(holder.pubkey.toBase58())!;
    sent = (await send(`queue close-out ${v.feed?.symbol} for ${t.wallet.toBase58().slice(0, 6)}`, [C.requestClose(payer.publicKey, t.wallet, v.key, C.decodeBook(new Uint8Array(book.data)).nextId)], true)) || sent;
  }
  return sent;
}

/** Routine upkeep while no trade is queued: finalize parked sides, keep every listed asset within
 *  ~50 slots of the chain (one crank advances all of them by 10), and every tenth pass settle
 *  the positions that price moves left out of date, so a fill has little left to do. */
async function upkeep(vaults: C.Vault[], market: Uint8Array, slot: bigint, pass: number) {
  const assets = vaults.map(v => v.assetIndex);
  const resets = assets.flatMap(a => C.finalizeResets(market, a));
  if (resets.length) await send("finalize side reset", resets);
  const oldest = assets.map(a => C.decodeAsset(market, a).slotLast).reduce((m, t) => (t < m ? t : m), slot);
  const lag = Number(slot - oldest);
  const vehicle = vaults[0].lpPortfolio;
  if (pass % 10 === 0 && lag <= 60) {
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

async function serviceVault(v: C.Vault, now: number, snap: Snapshot): Promise<boolean> {
  if (!snap.book) {
    await send(`open book ${v.feed?.symbol}`, [C.openBook(payer.publicKey, v.key)]);
    return false;
  }
  let b = C.decodeBook(snap.book);
  const reqs = await requestsOf(v, b);
  const live = reqs.filter(r => now <= r.target + C.ROUTER_GRACE_SECS);

  // 1. Time-critical: once the earliest pending target has passed, post the first Pyth price at
  //    or after it (only while its requests can still fill), then fill at it right away.
  const waiting = live.filter(r => r.target > b.markPublish).map(r => r.target);
  const earliest = waiting.length ? Math.min(...waiting) : null;
  if (earliest !== null && now >= earliest + 1 && now < earliest + C.ROUTER_GRACE_SECS - 3) {
    const u = await firstAtOrAfter(v.feed!.id, earliest);
    if (u && u.publish > b.markPublish) {
      await advancePosted(v, u);
      const ba = await conn.getAccountInfo(C.bookAddress(v.key));
      if (ba) b = C.decodeBook(new Uint8Array(ba.data));
    }
  }

  // 2. Fill requests the mark is at. The cranks in front bring every asset involved to the
  //    current slot (which also walks Percolator's price to the mark) and settle the positions
  //    that leaves out of date; a miss is retried next pass.
  for (const r of live.filter(r => b.markPrev < r.target && r.target <= b.markPublish)) {
    const what = `${v.feed?.symbol} #${r.id}`;
    const plan = C.crankPlan(await C.marketSnapshot(conn), payer.publicKey, v.lpPortfolio, v.assetIndex, r.wallet);
    const k = Math.max(1, Math.ceil((plan.lag + LAND_SLOTS) / 10));
    if (k > 4) {
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
    await race(`${r.size === 0n ? "close out" : "fill"} ${what} size ${Number(r.size) / 1e6} at ${Number(b.markPrice) / 1e6} (${tuned.k} lag + ${tuned.settle.length} settle cranks)`, [version(tuned.k), version(tuned.k + 1)]);
  }

  // 3. Expire requests past their grace period.
  for (const r of reqs.filter(r => now > r.target + C.ROUTER_GRACE_SECS)) {
    await send(`expire ${v.feed?.symbol} #${r.id}`, [C.expireRequest(payer.publicKey, r)]);
  }

  // 4. Free: move the mark to Pyth's sponsored feed account when it is newer, never past a
  //    pending target.
  if (snap.feed && live.length === 0) {
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
  log("executor", payer.publicKey.toBase58(), "router", C.ROUTER.toBase58(), "hermes", HERMES);
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
      if (!queued && vaults.length && (await closeOut(vaults, market).catch(e => { log("error", "close-out", String(e?.message ?? e).slice(0, 140)); return false; }))) continue;
      if (!queued && vaults.length) await upkeep(vaults, market, slot, pass++).catch(e => log("error", "upkeep", String(e?.message ?? e).slice(0, 140)));
      waiting = queued;
      for (const [i, v] of vaults.entries()) {
        const snap = { market, slot, book: accs[2 + 2 * i] ? new Uint8Array(accs[2 + 2 * i]!.data) : null, feed: accs[3 + 2 * i] ? new Uint8Array(accs[3 + 2 * i]!.data) : null };
        busy = (await serviceVault(v, now, snap).catch(e => { log("error", v.feed?.symbol, String(e?.message ?? e).slice(0, 140)); return false; })) || busy;
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
    await new Promise(r => setTimeout(r, waiting ? 1000 : 3000));
  }
})();
