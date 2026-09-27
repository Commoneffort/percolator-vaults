// One-time catch-up for an idle asset that fell far behind the market clock: each crank advances
// accrual by at most 10 slots, so this sends batches of 30-crank transactions until it is current.
// Run: npx tsx scripts/catchup.ts <asset_index>
import { ComputeBudgetProgram, Connection, Keypair, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
(async () => {
  const idx = Number(process.argv[2]);
  const v = (await C.listVaults(conn)).find(x => x.assetIndex === idx && x.status === 1)!;
  for (;;) {
    const [m, slot] = await Promise.all([conn.getAccountInfo(C.MARKET, "confirmed"), conn.getSlot("confirmed")]);
    const lag = slot - Number(C.decodeAsset(new Uint8Array(m!.data), idx).slotLast);
    console.log("asset", idx, "lag", lag);
    if (lag <= 40) return;
    let left = Math.ceil(lag / 10);
    const { blockhash } = await conn.getLatestBlockhash();
    const sends: Promise<unknown>[] = [];
    for (let t = 0; t < 8 && left > 0; t++) {
      const n = Math.min(30, left); left -= n;
      const buf = Buffer.alloc(13); buf[0] = 5; buf.writeBigUInt64LE(BigInt(slot), 1); buf[9] = 1; buf.writeUInt16LE(idx, 10); buf[12] = 1;
      const ix = new TransactionInstruction({ programId: C.PERCOLATOR, data: buf, keys: [
        { pubkey: payer.publicKey, isSigner: true, isWritable: true }, { pubkey: C.MARKET, isSigner: false, isWritable: true },
        { pubkey: v.lpPortfolio, isSigner: false, isWritable: true }, { pubkey: v.oracle, isSigner: false, isWritable: false }] });
      const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 - t }), ...Array(n).fill(ix));
      tx.feePayer = payer.publicKey; tx.recentBlockhash = blockhash; tx.sign(payer);
      sends.push(conn.sendRawTransaction(tx.serialize(), { skipPreflight: true }).then(s => conn.confirmTransaction(s, "confirmed")).catch(e => console.log("err", String(e).slice(0, 100))));
    }
    await Promise.all(sends);
  }
})();
