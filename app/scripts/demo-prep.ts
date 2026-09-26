// Prepares the demo recording: a burner key with SOL for rent (the faucet supplies the USDC on
// camera), a feed that is not open yet, and the BTC market address.
import { Connection, Keypair, LAMPORTS_PER_SOL, SystemProgram, Transaction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
(async () => {
  const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
  const path = "../keys/seed/demo-video.json";
  const kp = Keypair.generate();
  fs.writeFileSync(path, JSON.stringify(Array.from(kp.secretKey)));
  const deployer = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(`${process.env.HOME}/.config/solana/percolator-test/deployer.json`, "utf8"))));
  const sig = await conn.sendTransaction(new Transaction().add(SystemProgram.transfer({ fromPubkey: deployer.publicKey, toPubkey: kp.publicKey, lamports: 0.45 * LAMPORTS_PER_SOL })), [deployer]);
  await conn.confirmTransaction(sig, "confirmed");
  let open = "";
  for (const sym of ["PYTH", "RAY", "JUP", "WIF"]) {
    const f = C.FEEDS.find(x => x.symbol === sym)!;
    if (!(await conn.getAccountInfo(C.canonicalVaultAddress(f.id)))) { open = sym; break; }
  }
  const btc = C.canonicalVaultAddress(C.FEEDS.find(x => x.symbol === "BTC")!.id).toBase58();
  fs.writeFileSync("../demo/build/markets.json", JSON.stringify({ open, btc, wallet: kp.publicKey.toBase58() }));
  console.log({ open, btc, wallet: kp.publicKey.toBase58() });
})();
