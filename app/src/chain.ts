// Account decoding and instruction building for the vault program and Percolator. Offsets come
// from layout.json, which a Rust test exports from the program and engine types.
import {
  AccountMeta,
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
  TransactionMessage,
  VersionedTransaction,
} from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync } from "@solana/spl-token";
import L from "./layout.json";
import M from "./devnet.json";

export const RPC_URL: string = (import.meta as any).env?.VITE_RPC_URL ?? "https://api.devnet.solana.com";
export const VAULT_PROGRAM = new PublicKey(M.vault_program);
export const PERCOLATOR = new PublicKey(M.percolator_program);
export const MARKET = new PublicKey(M.market);
export const MINT = new PublicKey(M.collateral_mint);
export const PERC_VAULT = new PublicKey(M.percolator_vault);
export const PERC_VAULT_AUTH = new PublicKey(M.percolator_vault_authority);
export const PUSH_ORACLE = new PublicKey("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");
export const USDC = 1_000_000;
/** Epoch length of every canonical vault on devnet (the program's CANON_EPOCH_LEN_SLOTS). */
export const CANON_EPOCH_LEN_SLOTS = 1_500;
export const UNIT = 1_000_000n; // Percolator POS_SCALE: 1 unit = 1 whole token
export const SHARE_DECIMALS = 9;
export const VAULT_LEN = L.vault_len;

/** The feeds on Pyth's free plan (the executor fetches their updates from Hermes), which have
 *  shared devnet accounts and fit a 6-decimal price. */
export const FEEDS = [
  { symbol: "SOL", name: "Solana", id: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d" },
  { symbol: "BTC", name: "Bitcoin", id: "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43" },
  { symbol: "ETH", name: "Ethereum", id: "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace" },
  { symbol: "PYTH", name: "Pyth Network", id: "0bbf28e9a841a1cc788f6a361b17ca072d0ea3098a1e5df1c3922d06719579ff" },
  { symbol: "DOGE", name: "Dogecoin", id: "dcef50dd0a4cd2dcc17e45df1676dcb336a11a61c69df7a0299b0150c672d25c" },
  { symbol: "HYPE", name: "Hyperliquid", id: "4279e31cc369bbcc2faf022b382b080e32a8e689ff20fbc530d2a603eb6cd98b" },
  { symbol: "XAU", name: "Gold", id: "765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2" },
  { symbol: "EUR", name: "Euro", id: "a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b" },
];
export type Feed = (typeof FEEDS)[number];
const hexBytes = (h: string) => Uint8Array.from(h.match(/../g)!.map(b => parseInt(b, 16)));
const toHex = (b: Uint8Array) => Array.from(b, x => x.toString(16).padStart(2, "0")).join("");
export const feedById = (hex: string): Feed | undefined => FEEDS.find(f => f.id === hex);
export const feedAccount = (hex: string) =>
  PublicKey.findProgramAddressSync([new Uint8Array([0, 0]), hexBytes(hex)], PUSH_ORACLE)[0];

// ---- little-endian readers ----
const dv = (d: Uint8Array) => new DataView(d.buffer, d.byteOffset, d.byteLength);
const u64 = (d: Uint8Array, o: number) => dv(d).getBigUint64(o, true);
const u32 = (d: Uint8Array, o: number) => dv(d).getUint32(o, true);
const u16 = (d: Uint8Array, o: number) => dv(d).getUint16(o, true);
const i64 = (d: Uint8Array, o: number) => dv(d).getBigInt64(o, true);
const u128 = (d: Uint8Array, o: number) => u64(d, o) + (u64(d, o + 8) << 64n);
const i128 = (d: Uint8Array, o: number) => {
  const v = u128(d, o);
  return v >= 1n << 127n ? v - (1n << 128n) : v;
};
const key = (d: Uint8Array, o: number) => new PublicKey(d.slice(o, o + 32));

export type Vault = {
  key: PublicKey;
  status: number;
  market: PublicKey;
  creator: PublicKey;
  seed: bigint;
  shareMint: PublicKey;
  buffer: PublicKey;
  escrow: PublicKey;
  lpPortfolio: PublicKey;
  delegate: PublicKey;
  assetIndex: number;
  feedHex: string;
  feed?: Feed;
  oracle: PublicKey;
  epoch: bigint;
  epochStart: bigint;
  epochLen: bigint;
  inventory: bigint;
  pendingDeposit: bigint;
  pendingWithdraw: bigint;
  reserved: bigint;
  lastNav: bigint;
  totalFills: bigint;
  feesHarvested: bigint;
  maxFill: bigint;
  maxInventory: bigint;
  insuranceFloor: bigint;
  createdSlot: bigint;
  canonical: boolean;
  positionNavBps: number;
  fillNavBps: number;
  openerFeesOwed: bigint;
  openerFeesTotal: bigint;
};

export function decodeVault(k: PublicKey, d: Uint8Array): Vault {
  const v = L.vault;
  const feedHex = toHex(d.slice(v.oracle_feeds, v.oracle_feeds + 32));
  return {
    key: k,
    status: d[v.status],
    market: key(d, v.market),
    creator: key(d, v.creator),
    seed: u64(d, v.seed),
    shareMint: key(d, v.share_mint),
    buffer: key(d, v.buffer),
    escrow: key(d, v.share_escrow),
    lpPortfolio: key(d, v.lp_portfolio),
    delegate: key(d, v.matcher_delegate),
    assetIndex: u16(d, v.asset_index),
    feedHex,
    feed: feedById(feedHex),
    oracle: feedAccount(feedHex),
    epoch: u64(d, v.epoch),
    epochStart: u64(d, v.epoch_start_slot),
    epochLen: u64(d, v.epoch_len_slots),
    inventory: i128(d, v.inventory),
    pendingDeposit: u64(d, v.pending_deposit_assets),
    pendingWithdraw: u64(d, v.pending_withdraw_shares),
    reserved: u64(d, v.reserved_assets),
    lastNav: u64(d, v.last_nav),
    totalFills: u64(d, v.total_fills),
    feesHarvested: u64(d, v.total_fees_harvested),
    maxFill: u128(d, v.max_fill_abs),
    maxInventory: u128(d, v.max_inventory_abs),
    insuranceFloor: u64(d, v.insurance_floor),
    createdSlot: u64(d, v.created_slot),
    canonical: d[v.vault_kind] === 1,
    positionNavBps: u16(d, v.position_nav_bps),
    fillNavBps: u16(d, v.fill_nav_bps),
    openerFeesOwed: u64(d, v.opener_fees_owed),
    openerFeesTotal: u64(d, v.opener_fees_total),
  };
}

export type Portfolio = { capital: bigint; pnl: bigint; id: bigint; sequence: bigint; positionEpoch: bigint; position: bigint };

/** A leg's position as Percolator counts it. The stored size is scaled by how much the side has
 *  been deleveraged since the leg was opened (after another position on the asset was reduced
 *  unilaterally), and is zero once the side has been emptied and reset. */
export function legPosition(market: Uint8Array, d: Uint8Array, base: number): bigint {
  const q = i128(d, base + L.leg.basis_pos_q);
  let abs = q < 0n ? -q : q;
  const long = d[base + L.leg.side] === 0;
  const a = L.market.slots + u32(d, base + L.leg.asset_index) * L.market.slot_len + L.market.engine;
  if (u64(d, base + L.leg.epoch_snap) !== u64(market, a + (long ? L.market.epoch_long : L.market.epoch_short))) abs = 0n;
  else {
    const now = u128(market, a + (long ? L.market.a_long : L.market.a_short)), then = u128(d, base + L.leg.a_basis);
    if (then > 0n && now !== then) abs = (abs * now + then - 1n) / then;
  }
  return long ? abs : -abs;
}

/** `market` gives the position as Percolator counts it (see `legPosition`); without it, the
 *  stored size. */
export function decodePortfolio(d: Uint8Array, asset: number, market?: Uint8Array): Portfolio {
  const p = L.portfolio;
  const control = u64(d, p.control);
  let position = 0n;
  for (let i = 0; i < 16; i++) {
    const base = p.legs + i * p.leg_len;
    if (d[base + L.leg.active] !== 1 || u32(d, base + L.leg.asset_index) !== asset) continue;
    if (market) { position = legPosition(market, d, base); continue; }
    const q = i128(d, base + L.leg.basis_pos_q);
    const abs = q < 0n ? -q : q;
    position = d[base + L.leg.side] === 0 ? abs : -abs;
  }
  return {
    capital: u128(d, p.capital),
    pnl: i128(d, p.pnl),
    id: u64(d, p.id),
    sequence: u64(d, p.sequence),
    positionEpoch: (control >> 1n) & ((1n << 49n) - 1n),
    position,
  };
}

export type Asset = { marketId: bigint; price: bigint; slotLast: bigint; oiLong: bigint; oiShort: bigint; insurance: bigint };

export function decodeAsset(market: Uint8Array, index: number): Asset {
  const m = L.market;
  const e = m.slots + index * m.slot_len + m.engine;
  if (e + m.ins_spent_short + 16 > market.length) return { marketId: 0n, price: 0n, slotLast: 0n, oiLong: 0n, oiShort: 0n, insurance: 0n };
  return {
    marketId: u64(market, e + m.market_id),
    price: u64(market, e + m.effective_price),
    slotLast: u64(market, e + m.slot_last),
    oiLong: u128(market, e + m.oi_long),
    oiShort: u128(market, e + m.oi_short),
    insurance:
      u128(market, e + m.ins_budget_long) + u128(market, e + m.ins_budget_short) -
      u128(market, e + m.ins_spent_long) - u128(market, e + m.ins_spent_short),
  };
}

const LIFECYCLE_RETIRED = 4;

export const marketHeader = (d: Uint8Array) => {
  const slots = u32(d, L.market_header.max_market_slots);
  // Percolator refuses to append a slot while a retired one is free, so a listing reuses the
  // first retired slot and only appends when none is.
  let listSlot = slots;
  for (let i = 1; i < slots; i++) {
    if (d[L.market.slots + i * L.market.slot_len + L.market.engine + L.market.lifecycle] === LIFECYCLE_RETIRED) {
      listSlot = i;
      break;
    }
  }
  return { nextMarketId: u64(d, L.market_header.next_market_id), slots, listSlot };
};

let slotSecs = 0.4;
/** Seconds per slot, as measured by `measureSlotSeconds` (0.4 until then). */
export const slotSeconds = () => slotSecs;
/** Measures the cluster's recent slot time; epochs and approvals are counted in slots. */
export async function measureSlotSeconds(conn: Connection) {
  try {
    const samples = await conn.getRecentPerformanceSamples(5);
    const slots = samples.reduce((a, s) => a + s.numSlots, 0);
    if (slots > 0) slotSecs = samples.reduce((a, s) => a + s.samplePeriodSecs, 0) / slots;
  } catch {
    // keep the previous estimate
  }
  return slotSecs;
}

/** Pyth price (e6 atoms per whole token) and publish time from a sponsored price account. */
export function decodePyth(d: Uint8Array) {
  let off = 8 + 32;
  off += d[off] === 1 ? 1 : 2;
  off += 32;
  const price = dv(d).getBigInt64(off, true);
  const expo = dv(d).getInt32(off + 16, true);
  const publish = Number(dv(d).getBigInt64(off + 20, true));
  const prev = Number(dv(d).getBigInt64(off + 28, true));
  const e6 = expo <= -6 ? price / 10n ** BigInt(-6 - expo) : price * 10n ** BigInt(expo + 6);
  return { e6, publish, prev };
}

export type Ticket = { epoch: bigint; deposit: bigint; withdraw: bigint };
export const decodeTicket = (d: Uint8Array): Ticket => ({
  epoch: u64(d, L.ticket.epoch),
  deposit: u64(d, L.ticket.deposit_assets),
  withdraw: u64(d, L.ticket.withdraw_shares),
});

// ---- PDAs ----
const le8 = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, true);
  return b;
};
const enc = (s: string) => new TextEncoder().encode(s);
export const vaultAddress = (creator: PublicKey, seed: bigint) =>
  PublicKey.findProgramAddressSync([enc("vault"), MARKET.toBytes(), creator.toBytes(), le8(seed)], VAULT_PROGRAM)[0];
/** The one vault (and so the one market) the program allows for a feed. */
export const canonicalVaultAddress = (feedHex: string) =>
  PublicKey.findProgramAddressSync([enc("canon"), MARKET.toBytes(), hexBytes(feedHex), le8(0n)], VAULT_PROGRAM)[0];
export const childAddress = (tag: string, vault: PublicKey) =>
  PublicKey.findProgramAddressSync([enc(tag), vault.toBytes()], VAULT_PROGRAM)[0];
export const ticketAddress = (v: PublicKey, user: PublicKey) =>
  PublicKey.findProgramAddressSync([enc("ticket"), v.toBytes(), user.toBytes()], VAULT_PROGRAM)[0];
export const epochAddress = (v: PublicKey, epoch: bigint) =>
  PublicKey.findProgramAddressSync([enc("epoch"), v.toBytes(), le8(epoch)], VAULT_PROGRAM)[0];
export const matcherDelegate = (portfolio: PublicKey, vault: PublicKey) =>
  PublicKey.findProgramAddressSync(
    [enc("matcher"), MARKET.toBytes(), portfolio.toBytes(), vault.toBytes(), VAULT_PROGRAM.toBytes(), vault.toBytes()],
    PERCOLATOR,
  )[0];
export const ata = (owner: PublicKey, mint: PublicKey) => getAssociatedTokenAddressSync(mint, owner);
/** Each wallet has one trading portfolio per market, at a deterministic address. */
export const PORTFOLIO_LEN = L.portfolio.len;

// ---- instruction data ----
class W {
  b: number[] = [];
  u8(v: number) { this.b.push(v & 0xff); return this; }
  u16(v: number) { this.b.push(v & 0xff, (v >> 8) & 0xff); return this; }
  u32(v: number) { for (let i = 0; i < 4; i++) this.b.push((v >>> (8 * i)) & 0xff); return this; }
  u64(v: bigint) { for (let i = 0n; i < 8n; i++) this.b.push(Number((v >> (8n * i)) & 0xffn)); return this; }
  i128(v: bigint) { const x = v < 0n ? (1n << 128n) + v : v; for (let i = 0n; i < 16n; i++) this.b.push(Number((x >> (8n * i)) & 0xffn)); return this; }
  bytes(b: Uint8Array) { this.b.push(...b); return this; }
  done() { return Buffer.from(this.b); }
}
const ro = (pubkey: PublicKey, isSigner = false): AccountMeta => ({ pubkey, isSigner, isWritable: false });
const rw = (pubkey: PublicKey, isSigner = false): AccountMeta => ({ pubkey, isSigner, isWritable: true });

/** InitVault in operate mode. The program replaces every parameter with its canonical template;
 *  only the feed and the listing-fee budget matter. */
export function initVault(payer: PublicKey, feedHex: string, frontier: bigint) {
  const vault = canonicalVaultAddress(feedHex);
  const portfolio = childAddress("portfolio", vault);
  const data = new W()
    .u8(16).u64(0n).u16(0).u16(10).u16(0).u16(10_000).u16(0)
    .i128(1n).i128(1n)
    .u64(1_500n).u64(216_000n).u64(frontier)
    .u8(1).u64(0n).u64(2n * BigInt(USDC))
    .u8(1).u8(0).u8(0).u32(0).u16(0)
    .u64(300n).u64(200n).u64(1n).u64(0n)
    .bytes(hexBytes(feedHex)).bytes(new Uint8Array(64))
    .u16(0).u16(0)
    .done();
  return {
    vault,
    ix: new TransactionInstruction({
      programId: VAULT_PROGRAM,
      keys: [
        rw(payer, true), ro(payer, true), rw(vault), rw(MARKET), ro(MINT),
        rw(childAddress("shares", vault)), rw(childAddress("buffer", vault)), rw(childAddress("escrow", vault)),
        rw(portfolio), ro(matcherDelegate(portfolio, vault)), ro(PERCOLATOR), ro(VAULT_PROGRAM), ro(TOKEN_PROGRAM_ID), ro(SystemProgram.programId),
      ],
      data,
    }),
  };
}

/** ListAsset: the vault activates its own asset and configures its Pyth feed. */
export function listAsset(payer: PublicKey, vault: PublicKey, feedHex: string, assetIndex: number, marketId: bigint, initialPrice: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [
      rw(payer, true), rw(ata(payer, MINT)), rw(vault), rw(MARKET), rw(childAddress("buffer", vault)), rw(PERC_VAULT),
      ro(TOKEN_PROGRAM_ID), ro(SystemProgram.programId), ro(PERCOLATOR), ro(feedAccount(feedHex)),
    ],
    data: new W().u8(26).u16(assetIndex).u64(marketId).u64(0n).u64(initialPrice).u64(1n).u64(0n).done(),
  });
}

export function requestDeposit(v: Vault, user: PublicKey, amount: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [rw(user, true), rw(v.key), rw(ticketAddress(v.key, user)), rw(ata(user, MINT)), rw(v.buffer), ro(TOKEN_PROGRAM_ID), ro(SystemProgram.programId)],
    data: new W().u8(17).u64(amount).done(),
  });
}

export function requestWithdraw(v: Vault, user: PublicKey, shares: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [rw(user, true), rw(v.key), rw(ticketAddress(v.key, user)), rw(ata(user, v.shareMint)), rw(v.escrow), ro(TOKEN_PROGRAM_ID), ro(SystemProgram.programId)],
    data: new W().u8(18).u64(shares).done(),
  });
}

export function claim(v: Vault, user: PublicKey, epoch: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [ro(user, true), rw(v.key), rw(ticketAddress(v.key, user)), ro(epochAddress(v.key, epoch)), rw(ata(user, MINT)), rw(ata(user, v.shareMint)), rw(v.buffer), rw(v.escrow), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(20).done(),
  });
}

export const OPENER_FEE_BPS = 1_000;

export function claimOpenerFees(v: Vault, opener: PublicKey) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [ro(opener, true), rw(v.key), rw(ata(opener, MINT)), rw(v.buffer), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(29).done(),
  });
}

/** The largest fill the vault would take right now in each direction (mirrors the matcher). */
export function depth(s: State): { long: bigint; short: bigint } {
  const v = s.vault;
  const price = s.asset.price;
  let maxFill = v.maxFill, maxInv = v.maxInventory;
  if (v.positionNavBps && price > 0n) {
    const cap = (bps: number) => (v.lastNav * BigInt(bps) / 10_000n) * UNIT / price;
    maxInv = maxInv < cap(v.positionNavBps) ? maxInv : cap(v.positionNavBps);
    maxFill = maxFill < cap(v.fillNavBps) ? maxFill : cap(v.fillNavBps);
  }
  const inv = v.inventory;
  const room = (lpSign: 1n | -1n) => {
    const reducing = inv !== 0n && (inv > 0n) !== (lpSign > 0n);
    const abs = inv < 0n ? -inv : inv;
    const r = reducing ? abs + maxInv : maxInv > abs ? maxInv - abs : 0n;
    return r < maxFill ? r : maxFill;
  };
  const epochOver = s.slot >= v.epochStart + v.epochLen && (v.pendingDeposit > 0n || v.pendingWithdraw > 0n);
  if (epochOver) {
    const abs = inv < 0n ? -inv : inv;
    const f = abs < maxFill ? abs : maxFill;
    // Reduce-only: a taker long is only filled if the vault is long (and vice versa).
    return { long: inv > 0n ? f : 0n, short: inv < 0n ? f : 0n };
  }
  // A taker long moves the vault short (lpSign -1).
  return { long: room(-1n), short: room(1n) };
}

// ---- cranks ----

/** A `fetch` for the public RPC, which rate-limits per address: requests go out one at a time, at
 *  least `gapMs` apart, and a 429 is retried after a pause instead of failing the caller. */
export function politeFetch(gapMs = 120, retries = 4): typeof fetch {
  let queue: Promise<unknown> = Promise.resolve();
  let last = 0;
  const pause = (ms: number) => new Promise(r => setTimeout(r, ms));
  return (input, init) => {
    const run = async () => {
      for (let i = 0; ; i++) {
        const wait = last + gapMs - Date.now();
        if (wait > 0) await pause(wait);
        last = Date.now();
        const res = await fetch(input, init);
        if (res.status !== 429 || i >= retries) return res;
        await pause(700 * (i + 1));
      }
    };
    const p = queue.then(run, run);
    queue = p.catch(() => undefined);
    return p;
  };
}

/** What `crankPlan` reads: the market account, the slot it was read at, and every portfolio. */
export type MarketSnapshot = { market: Uint8Array; slot: number; portfolios: { pubkey: PublicKey; data: Uint8Array }[] };

export async function marketSnapshot(conn: Connection): Promise<MarketSnapshot> {
  const [m, slot, accs] = await Promise.all([
    conn.getAccountInfo(MARKET, "confirmed"),
    conn.getSlot("confirmed"),
    conn.getProgramAccounts(PERCOLATOR, { commitment: "confirmed", filters: [{ dataSize: PORTFOLIO_LEN }, { memcmp: { offset: 16, bytes: MARKET.toBase58() } }] }),
  ]);
  return { market: new Uint8Array(m!.data), slot, portfolios: accs.map(a => ({ pubkey: a.pubkey, data: new Uint8Array(a.account.data) })) };
}

/** Percolator's permissionless crank on `portfolio`, naming the assets it may accrue (each by at
 *  most 10 slots a call, up to the current slot). It fails (NonProgress) when it finds nothing to do. */
export const crank = (payer: PublicKey, portfolio: PublicKey, assets: number[]) => {
  const w = new W().u8(5).u64(0n).u8(assets.length);
  for (const a of assets) w.u16(a).u8(0);
  return new TransactionInstruction({ programId: PERCOLATOR, keys: [rw(payer, true), rw(MARKET), rw(portfolio)], data: w.done() });
};

/** The cranks Percolator needs before it accepts new risk on asset `focus`, as it works on chain:
 *   - an asset with positions must be accrued to the current slot, and one crank accrues every
 *     asset it names by at most 10 slots, so `ceil(lag / 10)` copies of `lagCrank` are needed,
 *     counted at the slot the transaction lands in: one too few leaves an asset out of date
 *     (the trade fails with LockActive), one too many has nothing to do (NonProgress);
 *   - every price move marks all positions on the asset out of date, and a portfolio is settled
 *     as a whole by one crank, which only works once every asset it holds is current. So the lag
 *     cranks cover `focus` and every asset held by anyone with a position on it (and by the
 *     trader), and `settle` has one crank for each of those portfolios that is, or is about to
 *     be, out of date. They end on `focus`, whose state decides whether the trade is accepted.
 *  With `focus` omitted the plan covers `assets` and every portfolio (routine upkeep). */
export type CrankPlan = { lag: number; lagCrank: TransactionInstruction; settle: TransactionInstruction[]; resets: TransactionInstruction[] };

export function crankPlan(s: MarketSnapshot, payer: PublicKey, vehicle: PublicKey, focus: number | undefined, trader?: PublicKey, assets: number[] = []): CrankPlan {
  const at = (a: number) => L.market.slots + a * L.market.slot_len + L.market.engine;
  const legs = (d: Uint8Array) => {
    const out: { asset: number; stale: boolean }[] = [];
    for (let i = 0; i < 16; i++) {
      const leg = L.portfolio.legs + i * L.portfolio.leg_len;
      if (d[leg + L.leg.active] !== 1) continue;
      const asset = u32(d, leg + L.leg.asset_index);
      const long = d[leg + L.leg.side] === 0;
      // Out of date after a price move, or left over from a side that was emptied and reset.
      const stale = u64(d, leg + L.leg.kf_epoch_snap) < u64(s.market, at(asset) + (long ? L.market.kf_epoch_long : L.market.kf_epoch_short))
        || u64(d, leg + L.leg.epoch_snap) !== u64(s.market, at(asset) + (long ? L.market.epoch_long : L.market.epoch_short));
      out.push({ asset, stale });
    }
    return out;
  };
  const mine = trader && routerPortfolio(trader);
  const held = s.portfolios.map(p => ({ key: p.pubkey, legs: legs(p.data) })).filter(p => p.legs.length);
  const holders = held.filter(p => focus === undefined || p.legs.some(l => l.asset === focus) || (mine && p.key.equals(mine)));
  // The trader first, holders of `focus` last, so the last asset touched is `focus`.
  holders.sort((x, y) => Number(mine ? y.key.equals(mine) : 0) - Number(mine ? x.key.equals(mine) : 0));
  const set = new Set<number>([...assets, ...holders.flatMap(p => p.legs.map(l => l.asset))]);
  if (focus !== undefined) { set.delete(focus); set.add(focus); }
  const all = [...set];
  const moving = (a: number) => u64(s.market, at(a) + L.market.effective_price) !== u64(s.market, at(a) + L.market.raw_oracle_target_price);
  const oldest = all.reduce((m, a) => { const t = u64(s.market, at(a) + L.market.slot_last); return t < m ? t : m; }, BigInt(s.slot));
  return {
    lag: s.slot - Number(oldest),
    lagCrank: crank(payer, vehicle, all),
    settle: holders.filter(p => p.legs.some(l => l.stale || moving(l.asset))).map(p => crank(payer, p.key, [focus ?? p.legs[0].asset])),
    resets: focus === undefined ? all.flatMap(a => finalizeResets(s.market, a)) : finalizeResets(s.market, focus),
  };
}

/** When the last position on one side of an asset closes, Percolator parks that side ("reset
 *  pending") and refuses new positions on the asset until anyone finalizes the reset. Returns
 *  that instruction for each parked side that is ready (no stored or out-of-date positions). */
export function finalizeResets(market: Uint8Array, assetIndex: number): TransactionInstruction[] {
  const a = L.market.slots + assetIndex * L.market.slot_len + L.market.engine;
  const sides = [
    [L.market.mode_long, L.market.stored_pos_count_long, L.market.stale_account_count_long],
    [L.market.mode_short, L.market.stored_pos_count_short, L.market.stale_account_count_short],
  ];
  return sides.flatMap(([mode, stored, stale], side) =>
    market[a + mode] === 2 && u64(market, a + stored) === 0n && u64(market, a + stale) === 0n
      ? [new TransactionInstruction({ programId: PERCOLATOR, keys: [rw(MARKET)], data: new W().u8(45).u16(assetIndex).u8(side).done() })]
      : []);
}

/** The trader takes `sizeQ` (positive = long) against the vault through Percolator's TradeCpi. */
export async function listVaults(conn: Connection): Promise<Vault[]> {
  const accs = await conn.getProgramAccounts(VAULT_PROGRAM, { commitment: "confirmed", filters: [{ dataSize: VAULT_LEN }] });
  return accs
    .map(a => decodeVault(a.pubkey, new Uint8Array(a.account.data)))
    .filter(v => v.market.equals(MARKET) && v.feed && (v.status !== 0 || v.canonical))
    .sort((a, b) => Number(b.createdSlot - a.createdSlot));
}

export async function fetchVault(conn: Connection, vaultKey: PublicKey, user?: PublicKey) {
  const va = await conn.getAccountInfo(vaultKey, "confirmed");
  if (!va) throw new Error("vault not found");
  const v = decodeVault(vaultKey, new Uint8Array(va.data));
  // Traders trade through the router: their portfolio belongs to the router's address for them.
  const pf = user ? routerPortfolio(user) : undefined;
  const keys = [MARKET, v.lpPortfolio, v.buffer, v.shareMint, v.oracle, bookAddress(v.key)];
  if (user) keys.push(ticketAddress(v.key, user), ata(user, MINT), ata(user, v.shareMint), pf!, traderAddress(user));
  const [accs, slot] = await Promise.all([conn.getMultipleAccountsInfo(keys, "confirmed"), conn.getSlot("confirmed")]);
  const data = (i: number) => (accs[i] ? new Uint8Array(accs[i]!.data) : undefined);
  const tokenAmount = (d?: Uint8Array) => (d ? u64(d, 64) : 0n);
  const book = data(5) ? decodeBook(data(5)!) : undefined;
  const trader = user && data(10) ? decodeTrader(data(10)!) : undefined;
  let pending: RouterRequest | undefined;
  if (trader?.hasPending && trader.pendingVault.equals(v.key)) {
    const k = requestAddress(v.key, trader.pendingId);
    const ra = await conn.getAccountInfo(k, "confirmed");
    if (ra) pending = decodeRequest(k, new Uint8Array(ra.data));
  }
  return {
    slot: BigInt(slot),
    vault: v,
    asset: decodeAsset(data(0)!, v.assetIndex),
    lp: decodePortfolio(data(1)!, v.assetIndex, data(0)!),
    closeOnly: closeOnly(data(0)!, v.assetIndex),
    buffer: tokenAmount(data(2)),
    shareSupply: data(3) ? u64(data(3)!, 36) : 0n,
    pyth: data(4) ? decodePyth(data(4)!) : undefined,
    book,
    ticket: user && data(6) ? decodeTicket(data(6)!) : undefined,
    userCollateral: user ? tokenAmount(data(7)) : 0n,
    userShares: user ? tokenAmount(data(8)) : 0n,
    portfolioKey: pf,
    portfolio: user && data(9) ? decodePortfolio(data(9)!, v.assetIndex, data(0)!) : undefined,
    margin: user && data(9) ? marginRoom(data(0)!, data(9)!, v.assetIndex) : undefined,
    openPositions: user && data(9) ? openPositions(data(9)!) : 0,
    trader,
    pending,
  };
}
export type State = Awaited<ReturnType<typeof fetchVault>>;

/** True while a market is close-only: a position on it was reduced unilaterally (the vault's
 *  Unwind, a liquidation), which deleverages the other side, and Percolator then refuses every
 *  risk-increasing trade on it until all positions on it are closed and its sides reset. */
export const closeOnly = (market: Uint8Array, asset: number) => {
  const a = L.market.slots + asset * L.market.slot_len + L.market.engine;
  return u128(market, a + L.market.a_long) !== ADL_ONE || u128(market, a + L.market.a_short) !== ADL_ONE;
};
const ADL_ONE = 1_000_000_000_000_000n;

/** How many markets a portfolio holds a position on. Percolator only pays margin out of a
 *  portfolio with none. */
export function openPositions(portfolio: Uint8Array): number {
  let n = 0;
  for (let i = 0; i < 16; i++) if (portfolio[L.portfolio.legs + i * L.portfolio.leg_len + L.leg.active] === 1) n++;
  return n;
}

/** The largest position (absolute, in position units) on `asset` that the router's margin check
 *  at request time accepts for this portfolio: equity must cover initial margin and fees on every
 *  position after a further `stress_bps` price move, plus that move as a loss (the router's
 *  `margin_covers`). A trade that only shrinks a position is always accepted. */
export function marginRoom(market: Uint8Array, portfolio: Uint8Array, asset: number): bigint {
  const R = L.router, P = L.portfolio;
  const price = (a: number) => u64(market, L.market.slots + a * L.market.slot_len + L.market.engine + L.market.effective_price);
  let other = 0n;
  for (let i = 0; i < 16; i++) {
    const leg = P.legs + i * P.leg_len;
    if (portfolio[leg + L.leg.active] !== 1) continue;
    const a = u32(portfolio, leg + L.leg.asset_index);
    const q = legPosition(market, portfolio, leg);
    if (a !== asset) other += (q < 0n ? -q : q) * price(a) / UNIT;
  }
  const equity = u128(portfolio, P.capital) + i128(portfolio, P.pnl);
  const stress = BigInt(R.stress_bps), rate = u64(market, R.im_bps_off) + u64(market, R.fee_bps_off);
  // need(total) = total * perUnit / 1e8 + minimum
  const perUnit = (10_000n + stress) * rate + stress * 10_000n;
  const total = (equity - u128(market, R.min_im_off)) * 100_000_000n / perUnit - other;
  const p = price(asset);
  return total > 0n && p > 0n ? total * UNIT * 999n / (p * 1000n) : 0n;
}

export const navOf = (s: State) =>
  s.lp.capital + (s.lp.pnl > 0n ? s.lp.pnl : 0n) + s.buffer - s.vault.reserved - s.vault.pendingDeposit;

export async function fetchMarket(conn: Connection) {
  const d = new Uint8Array((await conn.getAccountInfo(MARKET, "confirmed"))!.data);
  return { data: d, ...marketHeader(d) };
}

// ---- router: the only way to trade. A request fills at the first Pyth price published at or
// after its target time (landing time + a fixed delay), so nobody can trade at a price they know.

const R = L.router;
export const ROUTER = new PublicKey(R.program);
export const ROUTER_DELAY_SECS = R.delay_secs;
export const ROUTER_GRACE_SECS = R.grace_secs;
export const ROUTER_BOND_LAMPORTS = R.bond_lamports;
export const TRADER_LEN = R.trader_len;
const rpda = (...seeds: (Uint8Array | Buffer)[]) => PublicKey.findProgramAddressSync(seeds, ROUTER)[0];
const u64le = (v: bigint) => new W().u64(v).done();
export const routerAuthority = () => rpda(Buffer.from("authority"));
export const traderAddress = (wallet: PublicKey) => rpda(Buffer.from("trader"), MARKET.toBuffer(), wallet.toBuffer());
export const routerPortfolio = (wallet: PublicKey) => rpda(Buffer.from("portfolio"), MARKET.toBuffer(), wallet.toBuffer());
export const routerCollateral = (wallet: PublicKey) => rpda(Buffer.from("collateral"), MARKET.toBuffer(), wallet.toBuffer());
export const bookAddress = (vault: PublicKey) => rpda(Buffer.from("book"), vault.toBuffer());
export const requestAddress = (vault: PublicKey, id: bigint) => rpda(Buffer.from("request"), vault.toBuffer(), u64le(id));

export type Book = { vault: PublicKey; len: number; markPublish: number; markPrev: number; markPrice: bigint; nextId: bigint; pending: { id: bigint; target: number }[] };
export function decodeBook(d: Uint8Array): Book {
  const b = R.book;
  const len = d[b.len];
  const pending = Array.from({ length: len }, (_, i) => ({ id: u64(d, b.pending_id + 8 * i), target: Number(i64(d, b.pending_target + 8 * i)) }));
  return { vault: key(d, b.vault), len, markPublish: Number(i64(d, b.mark_publish_time)), markPrev: Number(i64(d, b.mark_prev_publish_time)), markPrice: u64(d, b.mark_price), nextId: u64(d, b.next_id), pending };
}
export type RouterRequest = { key: PublicKey; vault: PublicKey; wallet: PublicKey; id: bigint; size: bigint; target: number };
export const decodeRequest = (k: PublicKey, d: Uint8Array): RouterRequest => ({
  key: k, vault: key(d, R.request.vault), wallet: key(d, R.request.wallet), id: u64(d, R.request.id), size: i128(d, R.request.size), target: Number(i64(d, R.request.target_time)),
});
export type Trader = { wallet: PublicKey; portfolio: PublicKey; collateral: PublicKey; hasPending: boolean; pendingVault: PublicKey; pendingId: bigint };
export const decodeTrader = (d: Uint8Array): Trader => ({
  wallet: key(d, R.trader.wallet), portfolio: key(d, R.trader.portfolio), collateral: key(d, R.trader.collateral),
  hasPending: d[R.trader.has_pending] === 1, pendingVault: key(d, R.trader.pending_vault), pendingId: u64(d, R.trader.pending_id),
});

/** An asset's next oracle observation sequence and its authority epoch (Percolator checks both). */
export function controlSequences(market: Uint8Array, asset: number) {
  const base = L.market.slots + asset * L.market.slot_len + L.market.control_sequences;
  return { nextObservation: u64(market, base + L.market.oracle_observation) + 1n, authorityEpoch: u64(market, base + L.market.authority_epoch) };
}

export const openTradingAccount = (wallet: PublicKey) => new TransactionInstruction({
  programId: ROUTER,
  keys: [rw(wallet, true), rw(traderAddress(wallet)), rw(routerPortfolio(wallet)), rw(routerCollateral(wallet)), rw(MARKET), ro(MINT), ro(PERCOLATOR), ro(SystemProgram.programId), ro(TOKEN_PROGRAM_ID)],
  data: new W().u8(0).done(),
});
export const routerDeposit = (wallet: PublicKey, amount: bigint) => new TransactionInstruction({
  programId: ROUTER,
  keys: [ro(wallet, true), ro(traderAddress(wallet)), rw(ata(wallet, MINT)), rw(routerCollateral(wallet)), rw(routerPortfolio(wallet)), rw(MARKET), rw(PERC_VAULT), ro(TOKEN_PROGRAM_ID), ro(PERCOLATOR)],
  data: new W().u8(1).u64(amount).done(),
});
export const routerWithdraw = (wallet: PublicKey, amount: bigint) => new TransactionInstruction({
  programId: ROUTER,
  keys: [ro(wallet, true), ro(traderAddress(wallet)), rw(routerPortfolio(wallet)), rw(MARKET), rw(routerCollateral(wallet)), rw(ata(wallet, MINT)), rw(PERC_VAULT), ro(PERC_VAULT_AUTH), ro(TOKEN_PROGRAM_ID), ro(PERCOLATOR)],
  data: new W().u8(2).u64(amount).done(),
});
export const openBook = (payer: PublicKey, vault: PublicKey) => new TransactionInstruction({
  programId: ROUTER,
  keys: [rw(payer, true), ro(vault), rw(bookAddress(vault)), ro(SystemProgram.programId)],
  data: new W().u8(3).done(),
});
/** Queues a trade; `id` is the book's next id. It cannot be cancelled. */
export const requestTrade = (wallet: PublicKey, vault: PublicKey, id: bigint, size: bigint) => new TransactionInstruction({
  programId: ROUTER,
  keys: [rw(wallet, true), rw(traderAddress(wallet)), ro(vault), rw(bookAddress(vault)), rw(requestAddress(vault, id)), ro(MARKET), ro(routerPortfolio(wallet)), ro(SystemProgram.programId)],
  data: new W().u8(4).i128(size).done(),
});
/** Queues the close-out of `wallet`'s position on a close-only market (`payer` may be anyone). */
export const requestClose = (payer: PublicKey, wallet: PublicKey, vault: PublicKey, id: bigint) => new TransactionInstruction({
  programId: ROUTER,
  keys: [rw(payer, true), rw(traderAddress(wallet)), ro(wallet), ro(vault), rw(bookAddress(vault)), rw(requestAddress(vault, id)), ro(MARKET), ro(SystemProgram.programId)],
  data: new W().u8(8).done(),
});
/** Moves a vault's mark to a verified Pyth update (a PriceUpdateV2 account). */
export const advanceMark = (v: Vault, pyth: PublicKey, market: Uint8Array) => {
  const s = controlSequences(market, v.assetIndex);
  return new TransactionInstruction({
    programId: ROUTER,
    keys: [ro(routerAuthority()), ro(v.key), rw(bookAddress(v.key)), rw(MARKET), ro(pyth), ro(VAULT_PROGRAM), ro(PERCOLATOR)],
    data: new W().u8(5).u64(s.nextObservation).u64(s.authorityEpoch).done(),
  });
};
/** Fills a request whose target the mark is at. Anyone can; the executor earns the bond. */
export const fillRequest = (executor: PublicKey, v: Vault, r: RouterRequest) => new TransactionInstruction({
  programId: ROUTER,
  keys: [
    rw(executor, true), rw(r.key), rw(traderAddress(r.wallet)), rw(r.wallet), rw(bookAddress(v.key)), rw(v.key), rw(MARKET),
    rw(routerPortfolio(r.wallet)), rw(v.lpPortfolio), ro(v.delegate), ro(routerAuthority()), ro(VAULT_PROGRAM), ro(PERCOLATOR),
  ],
  data: new W().u8(6).done(),
});
export const expireRequest = (caller: PublicKey, r: RouterRequest) => new TransactionInstruction({
  programId: ROUTER,
  keys: [ro(caller, true), rw(r.key), rw(traderAddress(r.wallet)), rw(r.wallet), rw(bookAddress(r.vault))],
  data: new W().u8(7).done(),
});
