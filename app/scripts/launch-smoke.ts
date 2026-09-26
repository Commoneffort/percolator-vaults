// Launches a market from a local keypair with the app's own builders, then deposits into it.
import { ComputeBudgetProgram, Connection, Keypair, Transaction, TransactionInstruction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(process.argv[2], "utf8"))));
const sym = process.argv[3] ?? "ETH";
const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
const send = async (label: string, ixs: TransactionInstruction[]) => {
  const sig = await conn.sendTransaction(new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), ...ixs), [kp]);
  await conn.confirmTransaction(sig, "confirmed");
  console.log("ok", label, sig.slice(0, 20));
};
(async () => {
  const feed = C.FEEDS.find(f => f.symbol === sym)!;
  const m = await C.fetchMarket(conn);
  const pyth = C.decodePyth(new Uint8Array((await conn.getAccountInfo(C.feedAccount(feed.id)))!.data));
  const seed = BigInt(Date.now());
  const { vault, ix } = C.initVault(kp.publicKey, { seed, feedHex: feed.id, maxFillUnits: 20n, maxInventoryUnits: 50n, epochLenSlots: 1500n, insuranceFloor: 10n * BigInt(C.USDC), frontier: m.nextMarketId });
  await send("init vault " + vault.toBase58(), [ix]);
  const m2 = await C.fetchMarket(conn);
  await send("list " + sym, [C.listAsset(kp.publicKey, vault, feed.id, m2.slots, m2.nextMarketId, pyth.e6)]);
  const s = await C.fetchVault(conn, vault, kp.publicKey);
  console.log("status", s.vault.status, "asset", s.vault.assetIndex, "symbol", s.vault.feed?.symbol, "price", s.asset.price.toString());
  await send("deposit 100", [C.requestDeposit(s.vault, kp.publicKey, 100n * BigInt(C.USDC))]);
  console.log("vaults listed:", (await C.listVaults(conn)).map(v => `${v.feed?.symbol}:${v.key.toBase58().slice(0, 6)}`).join(" "));
})().catch(e => { console.log("FAIL", e.message); console.log((e.logs ?? []).slice(-4).join("\n")); });
