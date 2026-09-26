// Drives the web app's transaction builders against devnet with a local keypair.
import { ComputeBudgetProgram, Connection, Keypair, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction } from "@solana/spl-token";
import * as fs from "fs";
import * as C from "./src/chain";

const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(process.argv[2], "utf8"))));
const conn = new Connection(process.env.RPC ?? C.RPC_URL, "confirmed");
const send = async (label: string, ixs: TransactionInstruction[]) => {
  try {
    const sig = await conn.sendTransaction(new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 1_000_000 }), ...ixs), [kp]);
    await conn.confirmTransaction(sig, "confirmed");
    console.log("ok  ", label, sig.slice(0, 16));
  } catch (e: any) {
    console.log("FAIL", label, e.message, (e.logs ?? []).slice(-3).join(" | "));
  }
};
const j = (o: any) => JSON.stringify(o, (_k, v) => (typeof v === "bigint" ? v.toString() : v));

(async () => {
  const me = kp.publicKey;
  const pf = await C.portfolioAddress(me);
  let s = await C.fetchState(conn, me, pf);
  if (!s.portfolio) {
    await send("create trading account", [
      SystemProgram.createAccountWithSeed({ fromPubkey: me, basePubkey: me, seed: C.PORTFOLIO_SEED, newAccountPubkey: pf, lamports: await conn.getMinimumBalanceForRentExemption(C.PORTFOLIO_LEN), space: C.PORTFOLIO_LEN, programId: C.PERCOLATOR }),
      C.initPortfolio(me, pf),
    ]);
    s = await C.fetchState(conn, me, pf);
  }
  await send("add 300 margin", [C.percDeposit(me, pf, s.portfolio!, 300n * 1_000_000n)]);
  s = await C.fetchState(conn, me, pf);
  await send("long 1.5 SOL", [C.tradeAgainstVault(me, pf, s.portfolio!, s.lp, s.asset, 1_500_000n)]);
  s = await C.fetchState(conn, me, pf);
  console.log("after long: position", s.portfolio!.position.toString(), "vault inventory", s.vault.inventory.toString());
  await send("close", [C.tradeAgainstVault(me, pf, s.portfolio!, s.lp, s.asset, -s.portfolio!.position)]);
  s = await C.fetchState(conn, me, pf);
  console.log("after close: position", s.portfolio!.position.toString(), "capital", s.portfolio!.capital.toString());
  await send("request deposit 200", [C.requestDeposit(me, 200n * 1_000_000n)]);
  s = await C.fetchState(conn, me, pf);
  console.log("ticket", j(s.ticket), "vault pending", s.vault.pendingDeposit.toString());
  void createAssociatedTokenAccountIdempotentInstruction;
})();
