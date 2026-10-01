// One-time catch-up for an asset that fell far behind the market clock (for example while no
// keeper was running): each crank advances accrual by at most 10 slots, so this sends batches of
// crank transactions until it is current. Vault assets are in authority-mark mode, so a crank
// reads no oracle account.
// Run: RPC=... npx tsx scripts/catchup.ts <asset_index>
import { ComputeBudgetProgram, Connection, Keypair, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
const conn = new Connection(process.env.RPC ?? C.RPC_URL, { commitment: "confirmed", disableRetryOnRateLimit: true, fetch: C.politeFetch() });
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));
process.on("unhandledRejection", () => {});
(async () => {
  const idx = Number(process.argv[2]);
  const v = (await C.listVaults(conn)).find(x => x.assetIndex === idx && x.status === 1 && x.market.equals(C.MARKET))!;
  for (;;) {
    const [m, slot] = await Promise.all([conn.getAccountInfo(C.MARKET, "confirmed"), conn.getSlot("confirmed")]).catch(() => [null, 0] as const);
    if (!m) { await sleep(3000); continue; }
    const a = C.decodeAsset(new Uint8Array(m.data), idx);
    const lag = slot - Number(a.slotLast);
    console.log("asset", idx, "lag", lag);
    if (lag <= 20) return;
    // Cranks on an asset with positions cost more compute, so fewer fit in a transaction.
    const perTx = a.oiLong !== 0n || a.oiShort !== 0n ? 12 : 30;
    let left = Math.ceil(lag / 10);
    const { blockhash } = await conn.getLatestBlockhash().catch(() => ({ blockhash: "" }));
    if (!blockhash) { await sleep(3000); continue; }
    const sigs: string[] = [];
    for (let t = 0; t < 6 && left > 0; t++) {
      const n = Math.min(perTx, left); left -= n;
      const ix = new TransactionInstruction({ programId: C.PERCOLATOR, data: Buffer.concat([Buffer.from([5]), (() => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(slot)); return b; })(), Buffer.from([1, idx & 0xff, idx >> 8, 0])]), keys: [
        { pubkey: payer.publicKey, isSigner: true, isWritable: true }, { pubkey: C.MARKET, isSigner: false, isWritable: true }, { pubkey: v.lpPortfolio, isSigner: false, isWritable: true }] });
      const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 - t }), ...Array(n).fill(ix));
      tx.feePayer = payer.publicKey; tx.recentBlockhash = blockhash; tx.sign(payer);
      sigs.push(await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true }).catch(() => ""));
      await sleep(250);
    }
    await sleep(4000);
  }
})();
