// Account decoding and instruction building for the vault program and Percolator. Offsets come
// from layout.json, which a Rust test exports from the program and engine types.
import {
  AccountMeta,
  Connection,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
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
export const UNIT = 1_000_000n; // Percolator POS_SCALE: 1 unit = 1 whole token
export const SHARE_DECIMALS = 9;
export const VAULT_LEN = L.vault_len;

/** Pyth feeds whose sponsored devnet accounts are kept fresh, and that fit a 6-decimal price. */
export const FEEDS = [
  { symbol: "SOL", name: "Solana", id: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d" },
  { symbol: "BTC", name: "Bitcoin", id: "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43" },
  { symbol: "ETH", name: "Ethereum", id: "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace" },
  { symbol: "JUP", name: "Jupiter", id: "0a0408d619e9380abad35060f9192039ed5042fa6f82301d0e48bb52be830996" },
  { symbol: "WIF", name: "dogwifhat", id: "4ca4beeca86f0d164160323817a4e42b10010a724c2217c6ee41b54cd4cc61fc" },
  { symbol: "PYTH", name: "Pyth Network", id: "0bbf28e9a841a1cc788f6a361b17ca072d0ea3098a1e5df1c3922d06719579ff" },
  { symbol: "RAY", name: "Raydium", id: "91568baa8beb53db23eb3fb7f22c6e8bd303d103919e19733f2bb642d3e7987a" },
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
  };
}

export type Portfolio = { capital: bigint; pnl: bigint; id: bigint; sequence: bigint; positionEpoch: bigint; position: bigint };

export function decodePortfolio(d: Uint8Array, asset: number): Portfolio {
  const p = L.portfolio;
  const control = u64(d, p.control);
  let position = 0n;
  for (let i = 0; i < 16; i++) {
    const base = p.legs + i * p.leg_len;
    if (d[base + L.leg.active] !== 1 || u32(d, base + L.leg.asset_index) !== asset) continue;
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

export const marketHeader = (d: Uint8Array) => ({
  nextMarketId: u64(d, L.market_header.next_market_id),
  slots: u32(d, L.market_header.max_market_slots),
});

/** Pyth price (e6 atoms per whole token) and publish time from a sponsored price account. */
export function decodePyth(d: Uint8Array) {
  let off = 8 + 32;
  off += d[off] === 1 ? 1 : 2;
  off += 32;
  const price = dv(d).getBigInt64(off, true);
  const expo = dv(d).getInt32(off + 16, true);
  const publish = Number(dv(d).getBigInt64(off + 20, true));
  const e6 = expo <= -6 ? price / 10n ** BigInt(-6 - expo) : price * 10n ** BigInt(expo + 6);
  return { e6, publish };
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
export const portfolioSeed = (asset: number) => `pv1-asset-${asset}`;
export const portfolioAddress = (owner: PublicKey, asset: number) => PublicKey.createWithSeed(owner, portfolioSeed(asset), PERCOLATOR);
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

// ---- Percolator (trader side) ----
export const initPortfolio = (owner: PublicKey, portfolio: PublicKey) =>
  new TransactionInstruction({ programId: PERCOLATOR, keys: [ro(owner, true), rw(MARKET), rw(portfolio)], data: new W().u8(1).done() });

export const percDeposit = (owner: PublicKey, portfolio: PublicKey, p: Portfolio, amount: bigint) =>
  new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [ro(owner, true), rw(MARKET), rw(portfolio), rw(ata(owner, MINT)), rw(PERC_VAULT), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(3).u64(p.id).u64(p.sequence).i128(amount).done(),
  });

export const percWithdraw = (owner: PublicKey, portfolio: PublicKey, p: Portfolio, amount: bigint) =>
  new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [ro(owner, true), rw(MARKET), rw(portfolio), rw(ata(owner, MINT)), rw(PERC_VAULT), ro(PERC_VAULT_AUTH), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(4).u64(p.id).u64(p.sequence).i128(amount).done(),
  });

/** The trader takes `sizeQ` (positive = long) against the vault through Percolator's TradeCpi. */
export const tradeAgainstVault = (v: Vault, owner: PublicKey, portfolio: PublicKey, taker: Portfolio, lp: Portfolio, asset: Asset, sizeQ: bigint) =>
  new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [rw(owner, true), rw(MARKET), rw(portfolio), rw(v.lpPortfolio), ro(VAULT_PROGRAM), rw(v.key), ro(v.delegate)],
    data: new W()
      .u8(10)
      .u64(taker.id).u64(taker.positionEpoch)
      .u64(lp.id).u64(lp.positionEpoch).u64(lp.sequence)
      .u16(v.assetIndex).u64(asset.marketId)
      .i128(sizeQ).u64(100n).u64(0n).u16(0) // accept at most 1% in trading fees
      .done(),
  });

// ---- reads ----
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
  const pf = user ? await portfolioAddress(user, v.assetIndex) : undefined;
  const keys = [MARKET, v.lpPortfolio, v.buffer, v.shareMint, v.oracle];
  if (user) keys.push(ticketAddress(v.key, user), ata(user, MINT), ata(user, v.shareMint), pf!);
  const [accs, slot] = await Promise.all([conn.getMultipleAccountsInfo(keys, "confirmed"), conn.getSlot("confirmed")]);
  const data = (i: number) => (accs[i] ? new Uint8Array(accs[i]!.data) : undefined);
  const tokenAmount = (d?: Uint8Array) => (d ? u64(d, 64) : 0n);
  return {
    slot: BigInt(slot),
    vault: v,
    asset: decodeAsset(data(0)!, v.assetIndex),
    lp: decodePortfolio(data(1)!, v.assetIndex),
    buffer: tokenAmount(data(2)),
    shareSupply: data(3) ? u64(data(3)!, 36) : 0n,
    pyth: data(4) ? decodePyth(data(4)!) : undefined,
    ticket: user && data(5) ? decodeTicket(data(5)!) : undefined,
    userCollateral: user ? tokenAmount(data(6)) : 0n,
    userShares: user ? tokenAmount(data(7)) : 0n,
    portfolioKey: pf,
    portfolio: user && data(8) ? decodePortfolio(data(8)!, v.assetIndex) : undefined,
  };
}
export type State = Awaited<ReturnType<typeof fetchVault>>;

export const navOf = (s: State) =>
  s.lp.capital + (s.lp.pnl > 0n ? s.lp.pnl : 0n) + s.buffer - s.vault.reserved - s.vault.pendingDeposit;

export async function fetchMarket(conn: Connection) {
  const d = new Uint8Array((await conn.getAccountInfo(MARKET, "confirmed"))!.data);
  return { data: d, ...marketHeader(d) };
}
