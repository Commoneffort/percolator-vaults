// Account decoding and instruction building for the vault and Percolator. Offsets come from
// layout.json, which a Rust test exports from the program and engine types, so this file cannot
// drift from the on-chain layout without that test changing it.
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
export const VAULT = new PublicKey(M.vault);
export const MINT = new PublicKey(M.collateral_mint);
export const SHARE_MINT = new PublicKey(M.share_mint);
export const BUFFER = new PublicKey(M.buffer);
export const ESCROW = new PublicKey(M.escrow);
export const LP_PORTFOLIO = new PublicKey(M.lp_portfolio);
export const DELEGATE = new PublicKey(M.matcher_delegate);
export const PERC_VAULT = new PublicKey(M.percolator_vault);
export const PERC_VAULT_AUTH = new PublicKey(M.percolator_vault_authority);
export const ORACLE = new PublicKey(M.oracle_account);
export const ASSET = M.asset_index as number;
export const USDC = 1_000_000;
export const UNIT = 1_000_000n; // Percolator POS_SCALE: 1 unit = 1 SOL here
export const SHARE_DECIMALS = 9;

// ---- little-endian readers ----
const u64 = (d: Uint8Array, o: number) => new DataView(d.buffer, d.byteOffset).getBigUint64(o, true);
const u16 = (d: Uint8Array, o: number) => new DataView(d.buffer, d.byteOffset).getUint16(o, true);
const u128 = (d: Uint8Array, o: number) => u64(d, o) + (u64(d, o + 8) << 64n);
const i128 = (d: Uint8Array, o: number) => {
  const v = u128(d, o);
  return v >= 1n << 127n ? v - (1n << 128n) : v;
};
const key = (d: Uint8Array, o: number) => new PublicKey(d.slice(o, o + 32));

export type Vault = {
  status: number;
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
  spreadBps: number;
  maxInventory: bigint;
  insuranceFloor: bigint;
  assetIndex: number;
};

export function decodeVault(d: Uint8Array): Vault {
  const v = L.vault;
  return {
    status: d[v.status],
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
    spreadBps: u16(d, v.spread_bps),
    maxInventory: u128(d, v.max_inventory_abs),
    insuranceFloor: u64(d, v.insurance_floor),
    assetIndex: u16(d, v.asset_index),
  };
}

export type Portfolio = {
  capital: bigint;
  pnl: bigint;
  id: bigint;
  sequence: bigint;
  positionEpoch: bigint;
  position: bigint; // signed, in q units for ASSET
  owner: PublicKey;
};

export function decodePortfolio(d: Uint8Array): Portfolio {
  const p = L.portfolio;
  const control = u64(d, p.control);
  let position = 0n;
  for (let i = 0; i < 16; i++) {
    const base = p.legs + i * p.leg_len;
    if (d[base + L.leg.active] !== 1) continue;
    if (new DataView(d.buffer, d.byteOffset).getUint32(base + L.leg.asset_index, true) !== ASSET) continue;
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
    owner: key(d, p.owner),
  };
}

export type Asset = { marketId: bigint; price: bigint; slotLast: bigint; oiLong: bigint; oiShort: bigint; insurance: bigint };

export function decodeAsset(market: Uint8Array, index = ASSET): Asset {
  const m = L.market;
  const e = m.slots + index * m.slot_len + m.engine;
  const ins =
    u128(market, e + m.ins_budget_long) + u128(market, e + m.ins_budget_short) -
    u128(market, e + m.ins_spent_long) - u128(market, e + m.ins_spent_short);
  return {
    marketId: u64(market, e + m.market_id),
    price: u64(market, e + m.effective_price),
    slotLast: u64(market, e + m.slot_last),
    oiLong: u128(market, e + m.oi_long),
    oiShort: u128(market, e + m.oi_short),
    insurance: ins,
  };
}

export type Ticket = { epoch: bigint; deposit: bigint; withdraw: bigint };
export function decodeTicket(d: Uint8Array): Ticket {
  const t = L.ticket;
  return { epoch: u64(d, t.epoch), deposit: u64(d, t.deposit_assets), withdraw: u64(d, t.withdraw_shares) };
}

// ---- PDAs ----
const le8 = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, true);
  return b;
};
export const ticketAddress = (user: PublicKey) =>
  PublicKey.findProgramAddressSync([Buffer.from("ticket"), VAULT.toBuffer(), user.toBuffer()], VAULT_PROGRAM)[0];
export const epochAddress = (epoch: bigint) =>
  PublicKey.findProgramAddressSync([Buffer.from("epoch"), VAULT.toBuffer(), le8(epoch)], VAULT_PROGRAM)[0];
export const ata = (owner: PublicKey, mint: PublicKey) => getAssociatedTokenAddressSync(mint, owner);

// ---- instruction data ----
class W {
  b: number[] = [];
  u8(v: number) { this.b.push(v & 0xff); return this; }
  u16(v: number) { this.b.push(v & 0xff, (v >> 8) & 0xff); return this; }
  u64(v: bigint) { for (let i = 0n; i < 8n; i++) this.b.push(Number((v >> (8n * i)) & 0xffn)); return this; }
  i128(v: bigint) { const x = v < 0n ? (1n << 128n) + v : v; for (let i = 0n; i < 16n; i++) this.b.push(Number((x >> (8n * i)) & 0xffn)); return this; }
  done() { return Buffer.from(this.b); }
}
const ro = (pubkey: PublicKey, isSigner = false): AccountMeta => ({ pubkey, isSigner, isWritable: false });
const rw = (pubkey: PublicKey, isSigner = false): AccountMeta => ({ pubkey, isSigner, isWritable: true });

export function requestDeposit(user: PublicKey, amount: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [rw(user, true), rw(VAULT), rw(ticketAddress(user)), rw(ata(user, MINT)), rw(BUFFER), ro(TOKEN_PROGRAM_ID), ro(SystemProgram.programId)],
    data: new W().u8(17).u64(amount).done(),
  });
}

export function requestWithdraw(user: PublicKey, shares: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [rw(user, true), rw(VAULT), rw(ticketAddress(user)), rw(ata(user, SHARE_MINT)), rw(ESCROW), ro(TOKEN_PROGRAM_ID), ro(SystemProgram.programId)],
    data: new W().u8(18).u64(shares).done(),
  });
}

export function claim(user: PublicKey, epoch: bigint) {
  return new TransactionInstruction({
    programId: VAULT_PROGRAM,
    keys: [ro(user, true), rw(VAULT), rw(ticketAddress(user)), ro(epochAddress(epoch)), rw(ata(user, MINT)), rw(ata(user, SHARE_MINT)), rw(BUFFER), rw(ESCROW), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(20).done(),
  });
}

// ---- Percolator (trader side) ----
export function initPortfolio(owner: PublicKey, portfolio: PublicKey) {
  return new TransactionInstruction({ programId: PERCOLATOR, keys: [ro(owner, true), rw(MARKET), rw(portfolio)], data: new W().u8(1).done() });
}

export function percDeposit(owner: PublicKey, portfolio: PublicKey, p: Portfolio, amount: bigint) {
  return new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [ro(owner, true), rw(MARKET), rw(portfolio), rw(ata(owner, MINT)), rw(PERC_VAULT), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(3).u64(p.id).u64(p.sequence).i128(amount).done(),
  });
}

export function percWithdraw(owner: PublicKey, portfolio: PublicKey, p: Portfolio, amount: bigint) {
  return new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [ro(owner, true), rw(MARKET), rw(portfolio), rw(ata(owner, MINT)), rw(PERC_VAULT), ro(PERC_VAULT_AUTH), ro(TOKEN_PROGRAM_ID)],
    data: new W().u8(4).u64(p.id).u64(p.sequence).i128(amount).done(),
  });
}

export function crank(payer: PublicKey, target: PublicKey, nowSlot: bigint) {
  return new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [rw(payer, true), rw(MARKET), rw(target), ro(ORACLE)],
    data: new W().u8(5).u64(nowSlot).u8(1).u16(ASSET).u8(1).done(),
  });
}

/** The trader takes `sizeQ` (positive = long) against the vault through Percolator's TradeCpi. */
export function tradeAgainstVault(owner: PublicKey, portfolio: PublicKey, taker: Portfolio, lp: Portfolio, asset: Asset, sizeQ: bigint, limitPrice = 0n) {
  return new TransactionInstruction({
    programId: PERCOLATOR,
    keys: [rw(owner, true), rw(MARKET), rw(portfolio), rw(LP_PORTFOLIO), ro(VAULT_PROGRAM), rw(VAULT), ro(DELEGATE)],
    data: new W()
      .u8(10)
      .u64(taker.id).u64(taker.positionEpoch)
      .u64(lp.id).u64(lp.positionEpoch).u64(lp.sequence)
      .u16(ASSET).u64(asset.marketId)
      .i128(sizeQ).u64(100n).u64(limitPrice).u16(0) // accept at most 1% in trading fees
      .done(),
  });
}

// ---- reads ----
export async function fetchState(conn: Connection, user?: PublicKey, portfolio?: PublicKey) {
  const keys = [VAULT, MARKET, LP_PORTFOLIO, BUFFER, SHARE_MINT];
  if (user) keys.push(ticketAddress(user), ata(user, MINT), ata(user, SHARE_MINT));
  if (portfolio) keys.push(portfolio);
  const [accs, slot] = await Promise.all([conn.getMultipleAccountsInfo(keys, "confirmed"), conn.getSlot("confirmed")]);
  const data = (i: number) => (accs[i] ? new Uint8Array(accs[i]!.data) : undefined);
  const tokenAmount = (d?: Uint8Array) => (d ? u64(d, 64) : 0n);
  const mintSupply = (d?: Uint8Array) => (d ? u64(d, 36) : 0n);
  const vault = decodeVault(data(0)!);
  return {
    slot: BigInt(slot),
    vault,
    asset: decodeAsset(data(1)!, vault.assetIndex),
    lp: decodePortfolio(data(2)!),
    buffer: tokenAmount(data(3)),
    shareSupply: mintSupply(data(4)),
    ticket: user && data(5) ? decodeTicket(data(5)!) : undefined,
    userCollateral: user ? tokenAmount(data(6)) : 0n,
    userShares: user ? tokenAmount(data(7)) : 0n,
    portfolio: portfolio && data(user ? 8 : 5) ? decodePortfolio(data(user ? 8 : 5)!) : undefined,
  };
}
export type State = Awaited<ReturnType<typeof fetchState>>;

/** Each wallet's trading portfolio lives at a deterministic address, so it can always be found. */
export const PORTFOLIO_SEED = "percolator-vault-v1";
export const portfolioAddress = (owner: PublicKey) =>
  PublicKey.createWithSeed(owner, PORTFOLIO_SEED, PERCOLATOR);
export const PORTFOLIO_LEN = L.portfolio.len;
