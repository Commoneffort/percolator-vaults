// Devnet faucet: mints test USDC (and a little SOL for fees and rent) to a wallet.
// FAUCET_SECRET holds the faucet keypair (JSON byte array); it is the test USDC mint authority.
import { Connection, Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import { createAssociatedTokenAccountIdempotentInstruction, createMintToInstruction, getAssociatedTokenAddressSync } from "@solana/spl-token";
// Test USDC mint of the devnet market (deploy/devnet.json: collateral_mint).
const TEST_USDC_MINT = "FT7B8rR8NN1ciyGPmTDT8AxUUWtF1PGc4NCmuisYVPkh";

const USDC = 1_000_000;
const DRIP_USDC = 1_000;
const MAX_HELD_USDC = 5_000;
const DRIP_SOL = 0.3; // enough to launch a market (~0.12) or open a trading account (~0.07)

export default async function handler(req: any, res: any) {
  if (req.method !== "POST") return res.status(405).json({ error: "POST only" });
  let to: PublicKey;
  try {
    to = new PublicKey((typeof req.body === "string" ? JSON.parse(req.body) : req.body).address);
  } catch {
    return res.status(400).json({ error: "invalid address" });
  }
  const secret = process.env.FAUCET_SECRET;
  if (!secret) return res.status(500).json({ error: "faucet not configured" });
  const faucet = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(secret)));
  const conn = new Connection(process.env.RPC_URL ?? "https://api.devnet.solana.com", "confirmed");
  const mint = new PublicKey(TEST_USDC_MINT);
  const ata = getAssociatedTokenAddressSync(mint, to);

  const held = await conn.getTokenAccountBalance(ata).then(b => Number(b.value.amount) / USDC).catch(() => 0);
  if (held >= MAX_HELD_USDC) return res.status(429).json({ error: `you already hold ${held} test USDC` });
  const sol = (await conn.getBalance(to)) / LAMPORTS_PER_SOL;

  const tx = new Transaction().add(
    createAssociatedTokenAccountIdempotentInstruction(faucet.publicKey, ata, to, mint),
    createMintToInstruction(mint, ata, faucet.publicKey, BigInt(DRIP_USDC * USDC)),
  );
  const sendSol = sol < 0.2;
  if (sendSol) tx.add(SystemProgram.transfer({ fromPubkey: faucet.publicKey, toPubkey: to, lamports: DRIP_SOL * LAMPORTS_PER_SOL }));
  try {
    const sig = await conn.sendTransaction(tx, [faucet]);
    await conn.confirmTransaction(sig, "confirmed");
    return res.status(200).json({ usdc: DRIP_USDC, sol: sendSol ? DRIP_SOL : 0, sig });
  } catch (e: any) {
    return res.status(500).json({ error: e.message ?? String(e) });
  }
}
