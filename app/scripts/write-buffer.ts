// Fills an upgradeable-loader buffer with a program binary using throttled transactions (the
// public devnet RPC refuses the CLI's bursts), creating the buffer first if it does not exist.
// Only chunks that differ are (re)sent, so it can be re-run until the buffer matches.
// Run: RPC=... npx tsx scripts/write-buffer.ts <buffer-keypair.json> <program.so>
import { Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import { politeFetch } from "../src/chain";
const LOADER = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");
const HEADER = 37; // Buffer { authority: Option<Pubkey> }
const CHUNK = 900;
const conn = new Connection(process.env.RPC ?? "https://api.devnet.solana.com", { commitment: "confirmed", disableRetryOnRateLimit: true, fetch: politeFetch(250, 6) });
const payer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));
(async () => {
  const bufferKp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(process.argv[2], "utf8"))));
  const buffer = bufferKp.publicKey;
  const bin = fs.readFileSync(process.argv[3]);
  if (!(await conn.getAccountInfo(buffer))) {
    const space = HEADER + bin.length;
    const init = Buffer.alloc(4); // InitializeBuffer
    const tx = new Transaction().add(
      SystemProgram.createAccount({ fromPubkey: payer.publicKey, newAccountPubkey: buffer, lamports: await conn.getMinimumBalanceForRentExemption(space), space, programId: LOADER }),
      new TransactionInstruction({ programId: LOADER, data: init, keys: [{ pubkey: buffer, isSigner: false, isWritable: true }, { pubkey: payer.publicKey, isSigner: false, isWritable: false }] }),
    );
    console.log("created buffer", buffer.toBase58(), await conn.sendTransaction(tx, [payer, bufferKp]));
    await sleep(6000);
  }
  for (let pass = 0; pass < 20; pass++) {
    const acc = await conn.getAccountInfo(buffer).catch(() => null);
    if (!acc) { await sleep(3000); continue; }
    const have = acc.data.subarray(HEADER);
    const todo: number[] = [];
    for (let off = 0; off < bin.length; off += CHUNK) if (!have.subarray(off, off + CHUNK).equals(bin.subarray(off, Math.min(off + CHUNK, bin.length)))) todo.push(off);
    console.log(`pass ${pass}: ${todo.length} of ${Math.ceil(bin.length / CHUNK)} chunks to write`);
    if (!todo.length) { console.log("buffer matches binary"); return; }
    const { blockhash } = await conn.getLatestBlockhash().catch(() => ({ blockhash: "" }));
    if (!blockhash) { await sleep(3000); continue; }
    for (const off of todo) {
      const bytes = bin.subarray(off, Math.min(off + CHUNK, bin.length));
      const data = Buffer.alloc(4 + 4 + 8 + bytes.length);
      data.writeUInt32LE(1, 0); data.writeUInt32LE(off, 4); data.writeBigUInt64LE(BigInt(bytes.length), 8); bytes.copy(data, 16);
      const tx = new Transaction().add(new TransactionInstruction({ programId: LOADER, data, keys: [
        { pubkey: buffer, isSigner: false, isWritable: true }, { pubkey: payer.publicKey, isSigner: true, isWritable: false }] }));
      tx.feePayer = payer.publicKey; tx.recentBlockhash = blockhash; tx.sign(payer);
      await conn.sendRawTransaction(tx.serialize(), { skipPreflight: true }).catch(() => "");
    }
    await sleep(8000);
  }
})();
