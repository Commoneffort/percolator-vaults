// Router executor: fills queued trades at their target price and keeps every vault's mark fresh.
// Anyone can run one (the programs check everything); whoever fills a request earns its bond.
//
// For each live vault, every two seconds:
//   1. Fill every request whose target the mark is at, once Percolator's price has reached it
//      (with the cranks that get it there and settle positions the price move left stale).
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
import { ComputeBudgetProgram, Connection, Keypair, SYSVAR_CLOCK_PUBKEY, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import { createRequire } from "module";
import * as C from "../src/chain";

// Both SDKs are CommonJS; this package is an ES module.
const require = createRequire(import.meta.url);
const { Wallet } = require("@coral-xyz/anchor");
const { PythSolanaReceiver } = require("@pythnetwork/pyth-solana-receiver");

const conn = new Connection(process.env.RPC ?? C.RPC_URL, { commitment: "confirmed", disableRetryOnRateLimit: true });
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
  for (let i = 0; i < 60; i++) {
    await new Promise(r => setTimeout(r, 1000));
    const st = (await conn.getSignatureStatuses([sig]).catch(() => null))?.value?.[0];
    if (st?.err) throw new Error(`transaction failed: ${JSON.stringify(st.err)}`);
    if (st?.confirmationStatus === "confirmed" || st?.confirmationStatus === "finalized") return;
  }
  throw new Error("not confirmed within 60 s");
}

async function send(label: string, ixs: TransactionInstruction[]) {
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.setComputeUnitPrice({ microLamports: PRIORITY }), ...ixs);
  try {
    const sig = await conn.sendTransaction(tx, [payer]);
    await confirmed(sig);
    log("ok", label);
    return true;
  } catch (e: any) {
    const logs: string[] = e?.logs ?? [];
    log("fail", label, String(e?.message ?? e).split("\n")[0].slice(0, 120), logs.filter(l => /failed|error/.test(l)).slice(-1)[0] ?? "");
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

type Snapshot = { book: Uint8Array | null; feed: Uint8Array | null; market: Uint8Array };

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

  // 2. Fill requests the mark is at. Percolator's price walks to the mark as the asset accrues
  //    (prepareTrade adds those cranks and the settlement cranks); a miss is retried next pass.
  for (const r of live.filter(r => b.markPrev < r.target && r.target <= b.markPublish)) {
    const ixs = await C.prepareTrade(conn, v, payer.publicKey, C.fillRequest(payer.publicKey, v, r), vaultList);
    await send(`fill ${v.feed?.symbol} #${r.id} size ${Number(r.size) / 1e6} at ${Number(b.markPrice) / 1e6}`, ixs);
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
  for (;;) {
    try {
      // The vault list changes rarely; everything else is one batched read per pass (the public
      // devnet RPC allows about 100 requests per 10 seconds).
      if (!vaults.length || Date.now() - vaultsAt > 300_000) {
        vaultList = await C.listVaults(conn);
        vaults = vaultList.filter(v => v.market.equals(C.MARKET) && v.canonical && v.status === 1 && v.feed);
        vaultsAt = Date.now();
      }
      const keys = [SYSVAR_CLOCK_PUBKEY, C.MARKET, ...vaults.flatMap(v => [C.bookAddress(v.key), C.feedAccount(v.feed!.id)])];
      const accs = await conn.getMultipleAccountsInfo(keys);
      const now = Number(new DataView(accs[0]!.data.buffer, accs[0]!.data.byteOffset).getBigInt64(32, true));
      const market = new Uint8Array(accs[1]!.data);
      let busy = false;
      for (const [i, v] of vaults.entries()) {
        const snap = { market, book: accs[2 + 2 * i] ? new Uint8Array(accs[2 + 2 * i]!.data) : null, feed: accs[3 + 2 * i] ? new Uint8Array(accs[3 + 2 * i]!.data) : null };
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
    await new Promise(r => setTimeout(r, 3000));
  }
})();
