// Trader leaderboard for the devnet market, computed from chain data (no indexer):
// every trading portfolio's equity, its deposits and withdrawals from transaction history,
// and the volume of its trades against the vaults.
import { Connection, PublicKey } from "@solana/web3.js";
import L from "./_layout.js";
import { FEED_SYMBOLS, MARKET, PERCOLATOR, VAULT_PROGRAM } from "./_config.js";

const dv = (d: Uint8Array) => new DataView(d.buffer, d.byteOffset, d.byteLength);
const u64 = (d: Uint8Array, o: number) => dv(d).getBigUint64(o, true);
const u128 = (d: Uint8Array, o: number) => u64(d, o) + (u64(d, o + 8) << 64n);
const i128 = (d: Uint8Array, o: number) => { const v = u128(d, o); return v >= 1n << 127n ? v - (1n << 128n) : v; };
const hex = (b: Uint8Array) => Array.from(b, x => x.toString(16).padStart(2, "0")).join("");

type Flow = { deposits: bigint; withdrawals: bigint; trades: number; volumeByAsset: Map<number, bigint> };
const txCache = new Map<string, any>(); // survives between calls on a warm instance
let cached: { at: number; body: any } | undefined;

async function flowsFor(conn: Connection, portfolio: PublicKey): Promise<Flow> {
  const f: Flow = { deposits: 0n, withdrawals: 0n, trades: 0, volumeByAsset: new Map() };
  const sigs = (await conn.getSignaturesForAddress(portfolio, { limit: 300 }, "confirmed")).filter(s => !s.err).map(s => s.signature);
  const missing = sigs.filter(s => !txCache.has(s));
  // One request per transaction (free RPC plans reject batches), eight at a time.
  for (let i = 0; i < missing.length; i += 8) {
    const chunk = missing.slice(i, i + 8);
    const txs = await Promise.all(chunk.map(s => conn.getTransaction(s, { maxSupportedTransactionVersion: 0, commitment: "confirmed" }).catch(() => null)));
    txs.forEach((t, j) => t && txCache.set(chunk[j], t));
  }
  for (const s of sigs) {
    const tx = txCache.get(s);
    if (!tx) continue;
    const msg = tx.transaction.message;
    const keys: PublicKey[] = msg.staticAccountKeys ?? msg.accountKeys;
    for (const ix of msg.compiledInstructions ?? []) {
      if (!keys[ix.programIdIndex].equals(new PublicKey(PERCOLATOR))) continue;
      const data: Uint8Array = ix.data;
      const acct = (n: number) => keys[ix.accountKeyIndexes[n]];
      if (!acct(2)?.equals(portfolio)) continue;
      if (data[0] === 3) f.deposits += u128(data, 17);
      else if (data[0] === 4) f.withdrawals += u128(data, 17);
      else if (data[0] === 10) {
        const size = i128(data, 51);
        const asset = dv(data).getUint16(41, true);
        f.trades += 1;
        f.volumeByAsset.set(asset, (f.volumeByAsset.get(asset) ?? 0n) + (size < 0n ? -size : size));
      }
    }
  }
  return f;
}

async function build() {
  const conn = new Connection(process.env.RPC_URL ?? "https://api.devnet.solana.com", "confirmed");
  const P = L.portfolio;
  const [portfolios, vaults, market] = await Promise.all([
    conn.getProgramAccounts(new PublicKey(PERCOLATOR), { filters: [{ dataSize: P.len }, { memcmp: { offset: 16, bytes: MARKET } }] }),
    conn.getProgramAccounts(new PublicKey(VAULT_PROGRAM), { filters: [{ dataSize: L.vault_len }] }),
    conn.getAccountInfo(new PublicKey(MARKET)),
  ]);
  const vaultKeys = new Set(vaults.map(v => v.pubkey.toBase58()));
  const symbols = new Map<number, string>();
  for (const v of vaults) {
    const d = new Uint8Array(v.account.data);
    const sym = FEED_SYMBOLS[hex(d.slice(L.vault.oracle_feeds, L.vault.oracle_feeds + 32))];
    if (sym) symbols.set(dv(d).getUint16(L.vault.asset_index, true), sym);
  }
  const m = new Uint8Array(market!.data);
  const priceOf = (asset: number) => {
    const o = L.market.slots + asset * L.market.slot_len + L.market.engine + L.market.effective_price;
    return o + 8 <= m.length ? u64(m, o) : 0n;
  };

  const byWallet = new Map<string, any>();
  for (const p of portfolios) {
    const d = new Uint8Array(p.account.data);
    const owner = new PublicKey(d.slice(P.owner, P.owner + 32)).toBase58();
    if (vaultKeys.has(owner)) continue; // vault LP portfolios are market makers, not traders
    const f = await flowsFor(conn, p.pubkey);
    if (f.deposits === 0n && f.trades === 0) continue;
    const equity = u128(d, P.capital) + i128(d, P.pnl);
    const positions: { symbol: string; size: number }[] = [];
    for (let i = 0; i < 16; i++) {
      const b = P.legs + i * P.leg_len;
      if (d[b + L.leg.active] !== 1) continue;
      const asset = dv(d).getUint32(b + L.leg.asset_index, true);
      const q = i128(d, b + L.leg.basis_pos_q);
      const abs = Number(q < 0n ? -q : q) / 1e6;
      positions.push({ symbol: symbols.get(asset) ?? `#${asset}`, size: d[b + L.leg.side] === 0 ? abs : -abs });
    }
    let volume = 0;
    for (const [asset, q] of f.volumeByAsset) volume += (Number(q) / 1e6) * (Number(priceOf(asset)) / 1e6);
    const w = byWallet.get(owner) ?? { wallet: owner, equity: 0, deposited: 0, withdrawn: 0, volume: 0, trades: 0, positions: [] as any[], markets: new Set<string>() };
    w.equity += Number(equity) / 1e6;
    w.deposited += Number(f.deposits) / 1e6;
    w.withdrawn += Number(f.withdrawals) / 1e6;
    w.volume += volume;
    w.trades += f.trades;
    w.positions.push(...positions);
    for (const a of f.volumeByAsset.keys()) w.markets.add(symbols.get(a) ?? `#${a}`);
    byWallet.set(owner, w);
  }
  const rows = [...byWallet.values()].map(w => {
    const net = w.deposited - w.withdrawn;
    const pnl = w.equity - net;
    return { wallet: w.wallet, pnl, roi: w.deposited > 0 ? pnl / w.deposited : 0, volume: w.volume, trades: w.trades, equity: w.equity, markets: [...w.markets], positions: w.positions };
  });
  rows.sort((a, b) => b.pnl - a.pnl);
  return { updated: Date.now(), rows };
}

export default async function handler(_req: any, res: any) {
  try {
    if (!cached || Date.now() - cached.at > 30_000) cached = { at: Date.now(), body: await build() };
    res.setHeader("Cache-Control", "s-maxage=30, stale-while-revalidate=120");
    return res.status(200).json(cached.body);
  } catch (e: any) {
    return res.status(500).json({ error: e.message ?? String(e) });
  }
}
