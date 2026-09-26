// Opens the canonical market for a feed from a local keypair with the app's own builders,
// checks that a second vault for the same feed is impossible, then deposits.
import { ComputeBudgetProgram, Connection, Keypair, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(process.argv[2], "utf8"))));
const sym = process.argv[3] ?? "BTC";
const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
const send = async (label: string, ixs: TransactionInstruction[]) => {
  try {
    const sig = await conn.sendTransaction(new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), ...ixs), [kp]);
    await conn.confirmTransaction(sig, "confirmed");
    console.log("ok  ", label, sig.slice(0, 20));
    return true;
  } catch (e: any) { console.log("FAIL", label, String(e.message).split("\n")[0].slice(0, 140)); return false; }
};
(async () => {
  const feed = C.FEEDS.find(f => f.symbol === sym)!;
  const vault = C.canonicalVaultAddress(feed.id);
  const m = await C.fetchMarket(conn);
  await send(`create ${sym} vault ${vault.toBase58()}`, [C.initVault(kp.publicKey, feed.id, m.nextMarketId).ix]);
  await send(`create a SECOND ${sym} vault (must fail)`, [C.initVault(kp.publicKey, feed.id, m.nextMarketId).ix]);
  const m2 = await C.fetchMarket(conn);
  const pyth = C.decodePyth(new Uint8Array((await conn.getAccountInfo(C.feedAccount(feed.id)))!.data));
  await send(`list ${sym}`, [C.listAsset(kp.publicKey, vault, feed.id, m2.slots, m2.nextMarketId, pyth.e6)]);
  const s = await C.fetchVault(conn, vault, kp.publicKey);
  console.log("canonical", s.vault.canonical, "status", s.vault.status, "epochLen", s.vault.epochLen.toString(), "navBps", s.vault.positionNavBps, s.vault.fillNavBps, "asset", s.vault.assetIndex, "price", s.asset.price.toString());
  await send("deposit 200", [C.requestDeposit(s.vault, kp.publicKey, 200n * BigInt(C.USDC))]);
})();
