import { ComputeBudgetProgram, Connection, Keypair, Transaction } from "@solana/web3.js";
import * as fs from "fs";
import * as C from "../src/chain";
(async () => {
  const conn = new Connection(C.RPC_URL, "confirmed");
  const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync("../keys/seed/seed-trader-2.json", "utf8"))));
  const vault = C.canonicalVaultAddress(C.FEEDS[0].id);
  const s = await C.fetchVault(conn, vault, kp.publicKey);
  console.log("vault inv", s.vault.inventory.toString(), "lastNav", s.vault.lastNav.toString(), "epoch end in", (s.vault.epochStart + s.vault.epochLen - s.slot).toString(), "pending", s.vault.pendingDeposit.toString(), s.vault.pendingWithdraw.toString());
  console.log("trader capital", s.portfolio?.capital.toString(), "pos", s.portfolio?.position.toString(), "asset slotLast lag", (s.slot - s.asset.slotLast).toString(), "oi", s.asset.oiLong.toString());
  const cranks = await C.catchUpCranks(conn, s.vault, kp.publicKey);
  const tx = new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }), ...cranks, C.tradeAgainstVault(s.vault, kp.publicKey, s.portfolioKey!, s.portfolio!, s.lp, s.asset, -8_000_000n));
  tx.feePayer = kp.publicKey; tx.recentBlockhash = (await conn.getLatestBlockhash()).blockhash; tx.sign(kp);
  const r = await conn.simulateTransaction(tx);
  console.log("cranks", cranks.length, "err", JSON.stringify(r.value.err));
  console.log((r.value.logs ?? []).filter(l => /failed|error|consumed/.test(l)).slice(-6).join("\n"));
})();
