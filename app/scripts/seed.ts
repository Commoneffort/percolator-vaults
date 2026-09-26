// Seeds clearly labelled demo activity on devnet: one liquidity provider that opens markets and
// deposits, and three traders. Every wallet here is listed in api/_config.ts SEED_WALLETS, so the
// site tags its activity as seeded. Run: npx tsx scripts/seed.ts <phase>   (phase: setup | trade)
import { ComputeBudgetProgram, Connection, Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction, createMintToInstruction } from "@solana/spl-token";
import * as fs from "fs";
import * as C from "../src/chain";

const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
const KEYS = "../keys/seed";
const deployer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
const faucet = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync("../keys/faucet.json", "utf8"))));
const wallet = (name: string) => {
  const p = `${KEYS}/${name}.json`;
  if (!fs.existsSync(p)) fs.writeFileSync(p, JSON.stringify(Array.from(Keypair.generate().secretKey)));
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(p, "utf8"))));
};
const LP = wallet("seed-lp");
const TRADERS = [wallet("seed-trader-1"), wallet("seed-trader-2"), wallet("seed-trader-3")];
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));

async function send(label: string, kp: Keypair, ixs: TransactionInstruction[], extra: Keypair[] = []) {
  for (let i = 0; i < 3; i++) {
    try {
      const sig = await conn.sendTransaction(new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), ...ixs), [kp, ...extra]);
      await conn.confirmTransaction(sig, "confirmed");
      console.log("ok  ", label);
      return true;
    } catch (e: any) {
      const m = String(e.message).split("\n")[0].slice(0, 120);
      if (i === 2) { console.log("FAIL", label, m); return false; }
      await sleep(3000);
    }
  }
  return false;
}

async function fund(kp: Keypair, usdc: number, sol: number) {
  const ata = C.ata(kp.publicKey, C.MINT);
  const ixs: TransactionInstruction[] = [
    createAssociatedTokenAccountIdempotentInstruction(deployer.publicKey, ata, kp.publicKey, C.MINT),
    createMintToInstruction(C.MINT, ata, faucet.publicKey, BigInt(usdc * C.USDC)),
  ];
  const bal = await conn.getBalance(kp.publicKey);
  if (bal < sol * LAMPORTS_PER_SOL) ixs.push(SystemProgram.transfer({ fromPubkey: deployer.publicKey, toPubkey: kp.publicKey, lamports: Math.round(sol * LAMPORTS_PER_SOL) - bal }));
  await send(`fund ${kp.publicKey.toBase58().slice(0, 6)}`, deployer, ixs, [faucet]);
}

async function openMarket(sym: string) {
  const f = C.FEEDS.find(x => x.symbol === sym)!;
  const vault = C.canonicalVaultAddress(f.id);
  const acct = await conn.getAccountInfo(vault);
  if (!acct) {
    const m = await C.fetchMarket(conn);
    await send(`open ${sym}: create vault`, LP, [C.initVault(LP.publicKey, f.id, m.nextMarketId).ix]);
  }
  const v = C.decodeVault(vault, new Uint8Array((await conn.getAccountInfo(vault))!.data));
  if (v.status === 0) {
    const m = await C.fetchMarket(conn);
    const pyth = C.decodePyth(new Uint8Array((await conn.getAccountInfo(C.feedAccount(f.id)))!.data));
    await send(`open ${sym}: list`, LP, [C.listAsset(LP.publicKey, vault, f.id, m.slots, m.nextMarketId, pyth.e6)]);
  }
  return vault;
}

async function setup() {
  await fund(LP, 20_000, 0.6);
  for (const t of TRADERS) await fund(t, 3_000, 0.3);
  const deposits: Record<string, number> = { SOL: 6_000, BTC: 6_000, ETH: 5_000 };
  for (const [sym, amt] of Object.entries(deposits)) {
    const vault = await openMarket(sym);
    const s = await C.fetchVault(conn, vault, LP.publicKey);
    const t = s.ticket;
    if (t && (t.deposit > 0n || t.withdraw > 0n)) { console.log(`skip ${sym} deposit: already queued`); continue; }
    await send(`deposit ${amt} into ${sym}`, LP, [C.requestDeposit(s.vault, LP.publicKey, BigInt(amt * C.USDC))]);
  }
  console.log("wallets:", [LP, ...TRADERS].map(k => k.publicKey.toBase58()).join(" "));
}

async function portfolio(kp: Keypair, s: C.State) {
  const pf = await C.portfolioAddress(kp.publicKey, s.vault.assetIndex);
  if (!(await conn.getAccountInfo(pf))) {
    await send(`${kp.publicKey.toBase58().slice(0, 6)} account for asset ${s.vault.assetIndex}`, kp, [
      SystemProgram.createAccountWithSeed({ fromPubkey: kp.publicKey, basePubkey: kp.publicKey, seed: C.portfolioSeed(s.vault.assetIndex), newAccountPubkey: pf, lamports: await conn.getMinimumBalanceForRentExemption(C.PORTFOLIO_LEN), space: C.PORTFOLIO_LEN, programId: C.PERCOLATOR }),
      C.initPortfolio(kp.publicKey, pf),
    ]);
  }
  return pf;
}

async function trade(kp: Keypair, sym: string, size: number, margin: number) {
  const vault = C.canonicalVaultAddress(C.FEEDS.find(x => x.symbol === sym)!.id);
  let s = await C.fetchVault(conn, vault, kp.publicKey);
  const pf = await portfolio(kp, s);
  s = await C.fetchVault(conn, vault, kp.publicKey);
  if (margin > 0) {
    await send(`${kp.publicKey.toBase58().slice(0, 6)} margin ${margin} on ${sym}`, kp, [C.percDeposit(kp.publicKey, pf, s.portfolio!, BigInt(margin * C.USDC))]);
    s = await C.fetchVault(conn, vault, kp.publicKey);
  }
  const q = BigInt(Math.round(size * 1e6));
  const cranks = await C.catchUpCranks(conn, s.vault, kp.publicKey);
  await send(`${kp.publicKey.toBase58().slice(0, 6)} ${size > 0 ? "long" : "short"} ${Math.abs(size)} ${sym}`, kp, [...cranks, C.tradeAgainstVault(s.vault, kp.publicKey, pf, s.portfolio!, s.lp, s.asset, q)]);
}

async function closeAll(kp: Keypair, sym: string) {
  const vault = C.canonicalVaultAddress(C.FEEDS.find(x => x.symbol === sym)!.id);
  const s = await C.fetchVault(conn, vault, kp.publicKey);
  if (!s.portfolio || s.portfolio.position === 0n) return;
  const cranks = await C.catchUpCranks(conn, s.vault, kp.publicKey);
  await send(`${kp.publicKey.toBase58().slice(0, 6)} close ${sym}`, kp, [...cranks, C.tradeAgainstVault(s.vault, kp.publicKey, s.portfolioKey!, s.portfolio, s.lp, s.asset, -s.portfolio.position)]);
}

async function trades() {
  const [a, b, c] = TRADERS;
  await trade(a, "SOL", 12, 600);
  await trade(b, "SOL", -8, 500);
  await trade(a, "BTC", 0.02, 400);
  await trade(c, "ETH", 0.6, 500);
  await trade(b, "ETH", -0.4, 400);
  await trade(c, "BTC", -0.015, 300);
  await sleep(45_000);
  await closeAll(b, "SOL");
  await trade(a, "SOL", 4, 0);
  await closeAll(c, "ETH");
  await trade(c, "SOL", -5, 300);
}

(async () => {
  const phase = process.argv[2];
  if (phase === "setup") await setup();
  else if (phase === "trade") await trades();
  else console.log("usage: seed.ts setup | trade");
})();

export async function retry() {
  const [a, b, c] = TRADERS;
  await trade(b, "SOL", -8, 0);
  await trade(b, "ETH", -0.4, 0);
  await trade(c, "BTC", -0.015, 300);
}
if (process.argv[2] === "retry") retry();
