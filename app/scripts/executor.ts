// Router executor: keeps every vault's mark on fresh, verified Pyth prices and fills queued trades.
// Anyone can run one (the program checks everything); whoever fills a request earns its bond.
//
// For each live vault, every second:
//   1. Fill every request whose target the mark is at, once Percolator's price has reached it
//      (with the cranks that get it there and settle positions the price move left stale).
//   2. Expire requests nobody filled within the grace period (their bond is forfeited).
//   3. Move the mark: to the first Pyth price published at or after the earliest pending target,
//      once that exists; otherwise to the latest price every couple of seconds.
//
// Run: HERMES_URL=... HERMES_API_KEY=... RPC=... npx tsx scripts/executor.ts
import { ComputeBudgetProgram, Connection, Keypair, PublicKey, SYSVAR_CLOCK_PUBKEY, Transaction, TransactionInstruction } from "@solana/web3.js";
import { Wallet } from "@coral-xyz/anchor";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { HermesClient } from "@pythnetwork/hermes-client";
import * as fs from "fs";
import * as C from "../src/chain";

const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(
  process.env.KEYPAIR ?? `${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
const apiKey = process.env.HERMES_API_KEY;
const hermes = new HermesClient(process.env.HERMES_URL ?? "https://hermes.pyth.network", {
  headers: apiKey ? { Authorization: `Bearer ${apiKey}`, "x-api-key": apiKey } : undefined,
});
const receiver = new PythSolanaReceiver({ connection: conn, wallet: new Wallet(payer) });
const LATEST_EVERY_SECS = 2;
const log = (...a: unknown[]) => console.log(new Date().toISOString().slice(11, 19), ...a);

async function chainTime(): Promise<number> {
  const a = await conn.getAccountInfo(SYSVAR_CLOCK_PUBKEY);
  return Number(new DataView(a!.data.buffer, a!.data.byteOffset).getBigInt64(32, true));
}

async function send(label: string, ixs: TransactionInstruction[]) {
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 20_000 }), ...ixs);
  try {
    const sig = await conn.sendTransaction(tx, [payer], { skipPreflight: false });
    await conn.confirmTransaction(sig, "confirmed");
    log("ok", label, sig.slice(0, 16));
    return true;
  } catch (e: any) {
    const logs: string[] = e?.logs ?? [];
    log("fail", label, String(e?.message ?? e).split("\n")[0].slice(0, 140), logs.filter(l => /failed|error/.test(l)).slice(-1)[0] ?? "");
    return false;
  }
}

type Update = { vaa: string; publish: number; prev: number };

/** The first Pyth update for `feed` published at or after `t` (prev_publish_time < t <= publish_time). */
async function firstAtOrAfter(feed: string, t: number): Promise<Update | null> {
  for (let s = t; s < t + 5; s++) {
    const r: any = await hermes.getPriceUpdatesAtTimestamp(s, ["0x" + feed], { encoding: "base64", parsed: true });
    const p = r?.parsed?.[0];
    if (!p) continue;
    const u = { vaa: r.binary.data[0], publish: Number(p.price.publish_time), prev: Number(p.metadata.prev_publish_time) };
    if (u.prev < t && t <= u.publish) return u;
    if (u.publish >= t) return null; // Hermes skipped the first one; do not post a later price for this target
  }
  return null;
}

async function latest(feed: string): Promise<Update | null> {
  const r: any = await hermes.getLatestPriceUpdates(["0x" + feed], { encoding: "base64", parsed: true });
  const p = r?.parsed?.[0];
  return p ? { vaa: r.binary.data[0], publish: Number(p.price.publish_time), prev: Number(p.metadata.prev_publish_time) } : null;
}

/** Posts a verified update and moves the vault's mark to it, in one builder run. */
async function advance(v: C.Vault, u: Update) {
  const market = (await conn.getAccountInfo(C.MARKET))!.data;
  const builder = receiver.newTransactionBuilder({ closeUpdateAccounts: true });
  await builder.addPostPriceUpdates([u.vaa]);
  await builder.addPriceConsumerInstructions(async getAccount => [
    { instruction: C.advanceMark(v, getAccount("0x" + v.feed!.id), new Uint8Array(market)), signers: [] },
  ]);
  const txs = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: 20_000 });
  try {
    await receiver.provider.sendAll(txs, { skipPreflight: true });
    log("ok", `advance ${v.feed!.symbol} to ${u.publish}`);
  } catch (e: any) {
    log("fail", `advance ${v.feed!.symbol} to ${u.publish}`, String(e?.message ?? e).slice(0, 160));
  }
}

async function requestsOf(v: C.Vault, b: C.Book) {
  const keys = b.pending.map(p => C.requestAddress(v.key, p.id));
  const accs = keys.length ? await conn.getMultipleAccountsInfo(keys) : [];
  return accs.flatMap((a, i) => (a ? [C.decodeRequest(keys[i], new Uint8Array(a.data))] : []));
}

const lastLatest = new Map<string, number>();
let lastFeedRefresh = 0;

/** Keeps the shared (shard 0) Pyth feed accounts of feeds not listed yet under ~2 minutes old, so
 *  opening a market (which needs a price at most 300 s old) always works. */
async function refreshUnlistedFeeds(listed: Set<string>) {
  const wall = Math.floor(Date.now() / 1000);
  if (wall - lastFeedRefresh < 120) return;
  lastFeedRefresh = wall;
  const ids = C.FEEDS.filter(f => !listed.has(f.id)).map(f => "0x" + f.id);
  if (!ids.length) return;
  try {
    const r: any = await hermes.getLatestPriceUpdates(ids, { encoding: "base64" });
    const builder = receiver.newTransactionBuilder({ closeUpdateAccounts: true });
    await builder.addUpdatePriceFeed(r.binary.data, 0);
    await receiver.provider.sendAll(await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: 20_000 }), { skipPreflight: true });
    log("ok", `refreshed ${ids.length} unlisted feed accounts`);
  } catch (e: any) {
    log("fail", "refresh feeds", String(e?.message ?? e).slice(0, 160));
  }
}

async function serviceVault(v: C.Vault, now: number) {
  const bk = C.bookAddress(v.key);
  const ba = await conn.getAccountInfo(bk);
  if (!ba) {
    await send(`open book ${v.feed?.symbol}`, [C.openBook(payer.publicKey, v.key)]);
    return;
  }
  const b = C.decodeBook(new Uint8Array(ba.data));
  const reqs = await requestsOf(v, b);

  // 1. Fill requests the mark is at.
  const atMark = reqs.filter(r => b.markPrev < r.target && r.target <= b.markPublish);
  for (const r of atMark) {
    const ixs = await C.prepareTrade(conn, v, payer.publicKey, C.fillRequest(payer.publicKey, v, r));
    await send(`fill ${v.feed?.symbol} #${r.id} (${r.size})`, ixs);
  }

  // 2. Expire requests past their grace period.
  for (const r of reqs.filter(r => now > r.target + C.ROUTER_GRACE_SECS)) {
    await send(`expire ${v.feed?.symbol} #${r.id}`, [C.expireRequest(payer.publicKey, r)]);
  }

  // 3. Move the mark.
  const waiting = reqs.filter(r => r.target > b.markPublish).map(r => r.target);
  const earliest = waiting.length ? Math.min(...waiting) : null;
  const wall = Math.floor(Date.now() / 1000);
  if (earliest !== null && wall >= earliest + 1) {
    const u = await firstAtOrAfter(v.feed!.id, earliest);
    if (u && u.publish > b.markPublish) await advance(v, u);
    return;
  }
  if (wall - (lastLatest.get(v.key.toBase58()) ?? 0) >= LATEST_EVERY_SECS) {
    lastLatest.set(v.key.toBase58(), wall);
    const u = await latest(v.feed!.id);
    // Never past a pending target: the first price at it is what that request fills at.
    if (u && u.publish > b.markPublish && (earliest === null || u.publish < earliest)) await advance(v, u);
  }
}

(async () => {
  log("executor", payer.publicKey.toBase58(), "router", C.ROUTER.toBase58());
  for (;;) {
    try {
      const now = await chainTime();
      const vaults = (await C.listVaults(conn)).filter(v => v.canonical && v.status === 1 && v.feed);
      for (const v of vaults) await serviceVault(v, now).catch(e => log("error", v.feed?.symbol, String(e?.message ?? e).slice(0, 160)));
      await refreshUnlistedFeeds(new Set(vaults.map(v => v.feed!.id)));
    } catch (e: any) {
      log("error", String(e?.message ?? e).slice(0, 160));
    }
    await new Promise(r => setTimeout(r, 1000));
  }
})();
