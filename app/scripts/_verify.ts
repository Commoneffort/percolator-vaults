// Live checks on devnet: each vault's asset is in authority-mark mode with the vault as oracle
// authority, and a direct trade against a vault is refused by its matcher (NotArmed).
import { ComputeBudgetProgram, Connection, Keypair, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
(async () => {
  const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
  const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync("../keys/seed/seed-trader-1.json", "utf8"))));
  const vaults = (await C.listVaults(conn)).filter(v => v.market.equals(C.MARKET));
  for (const v of vaults) console.log(v.feed?.symbol, "asset", v.assetIndex, "status", v.status, "book", !!(await conn.getAccountInfo(C.bookAddress(v.key))));
  // A plain wallet-owned Percolator portfolio trying TradeCpi against the SOL vault.
  const v = vaults.find(x => x.feed?.symbol === "SOL")!;
  const pf = await (await import("@solana/web3.js")).PublicKey.createWithSeed(kp.publicKey, "direct-0", C.PERCOLATOR);
  const ixs: TransactionInstruction[] = [];
  const w = (n: number) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
  if (!(await conn.getAccountInfo(pf))) ixs.push(
    SystemProgram.createAccountWithSeed({ fromPubkey: kp.publicKey, basePubkey: kp.publicKey, seed: "direct-0", newAccountPubkey: pf, lamports: await conn.getMinimumBalanceForRentExemption(C.PORTFOLIO_LEN), space: C.PORTFOLIO_LEN, programId: C.PERCOLATOR }),
    new TransactionInstruction({ programId: C.PERCOLATOR, keys: [{ pubkey: kp.publicKey, isSigner: true, isWritable: false }, { pubkey: C.MARKET, isSigner: false, isWritable: true }, { pubkey: pf, isSigner: false, isWritable: true }], data: Buffer.from([1]) }),
  );
  if (ixs.length) {
    const t = new Transaction().add(...ixs);
    await conn.confirmTransaction(await conn.sendTransaction(t, [kp]), "confirmed");
    ixs.length = 0;
  }
  // Fund it (Percolator Deposit from the wallet's own token account).
  const pd = new Uint8Array((await conn.getAccountInfo(pf))!.data);
  const L = (await import("../src/layout.json")).default as any;
  const rdu = (o: number) => new DataView(pd.buffer, pd.byteOffset).getBigUint64(o, true);
  const pid = rdu(L.portfolio.id), pseq = rdu(L.portfolio.sequence), pepoch = (rdu(L.portfolio.control) >> 1n) & ((1n << 49n) - 1n);
  if (C.decodePortfolio(pd, v.assetIndex).capital === 0n) {
    const dep = Buffer.concat([Buffer.from([3]), w(Number(pid)), w(Number(pseq)), Buffer.from(new BigUint64Array([100_000_000n, 0n]).buffer)]);
    const t = new Transaction().add(new TransactionInstruction({ programId: C.PERCOLATOR, data: dep, keys: [
      { pubkey: kp.publicKey, isSigner: true, isWritable: false }, { pubkey: C.MARKET, isSigner: false, isWritable: true }, { pubkey: pf, isSigner: false, isWritable: true },
      { pubkey: C.ata(kp.publicKey, C.MINT), isSigner: false, isWritable: true }, { pubkey: C.PERC_VAULT, isSigner: false, isWritable: true }, { pubkey: new (await import("@solana/web3.js")).PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), isSigner: false, isWritable: false }] }));
    await conn.confirmTransaction(await conn.sendTransaction(t, [kp]), "confirmed");
  }
  const s = await C.fetchVault(conn, v.key);
  const data = Buffer.concat([Buffer.from([10]), w(Number(pid)), w(Number(pepoch)), w(Number(s.lp.id)), w(Number(s.lp.positionEpoch)), w(Number(s.lp.sequence)), Buffer.from(new Uint16Array([v.assetIndex]).buffer), w(Number(s.asset.marketId)), Buffer.from(new BigInt64Array([1_000_000n, 0n]).buffer), w(10_000), w(0), Buffer.alloc(2)]);
  ixs.push(new TransactionInstruction({ programId: C.PERCOLATOR, data, keys: [
    { pubkey: kp.publicKey, isSigner: true, isWritable: true }, { pubkey: C.MARKET, isSigner: false, isWritable: true }, { pubkey: pf, isSigner: false, isWritable: true },
    { pubkey: v.lpPortfolio, isSigner: false, isWritable: true }, { pubkey: C.VAULT_PROGRAM, isSigner: false, isWritable: false }, { pubkey: v.key, isSigner: false, isWritable: true }, { pubkey: v.delegate, isSigner: false, isWritable: false }] }));
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ...ixs);
  tx.feePayer = kp.publicKey; tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash; tx.sign(kp);
  const r = await conn.simulateTransaction(tx);
  console.log("direct trade:", JSON.stringify(r.value.err), (r.value.logs ?? []).filter(l => /BSync.*failed|custom program error/.test(l)).slice(0, 2).join(" | "));
})();
