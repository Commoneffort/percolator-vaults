// Keeps a Pyth push-feed account fresh on devnet: fetches the latest signed update from Hermes
// and posts it to the Pyth receiver (shard 0), every INTERVAL seconds.
import { Connection, Keypair } from "@solana/web3.js";
import { Wallet } from "@coral-xyz/anchor";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { HermesClient } from "@pythnetwork/hermes-client";
import * as fs from "fs";

const RPC = process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";
const KEYPAIR = process.env.KEYPAIR ?? `${process.env.HOME}/.config/solana/percolator-test/deployer.json`;
const FEED = process.env.FEED ?? "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const INTERVAL = Number(process.env.INTERVAL ?? "60");

async function pushOnce(receiver: PythSolanaReceiver, hermes: HermesClient) {
  const upd = await hermes.getLatestPriceUpdates(["0x" + FEED], { encoding: "base64" });
  const px = upd.parsed?.[0]?.price;
  const builder = receiver.newTransactionBuilder({ closeUpdateAccounts: true });
  await builder.addUpdatePriceFeed(upd.binary.data, 0);
  const txs = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: 20_000 });
  const sigs = await receiver.provider.sendAll(txs, { skipPreflight: true });
  console.log(new Date().toISOString(), `price ${px?.price} e${px?.expo} publish ${px?.publish_time}`, sigs.at(-1));
}

async function main() {
  const connection = new Connection(RPC, "confirmed");
  const kp = Keypair.fromSecretKey(new Uint8Array(JSON.parse(fs.readFileSync(KEYPAIR, "utf8"))));
  const receiver = new PythSolanaReceiver({ connection, wallet: new Wallet(kp) });
  const hermes = new HermesClient("https://hermes.pyth.network");
  console.log("feed account:", receiver.getPriceFeedAccountAddress(0, "0x" + FEED).toBase58());
  for (;;) {
    try { await pushOnce(receiver, hermes); } catch (e: any) { console.error("push failed:", e.message ?? e); }
    if (process.env.ONCE) return;
    await new Promise(r => setTimeout(r, INTERVAL * 1000));
  }
}
main();
