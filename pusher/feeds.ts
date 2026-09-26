import { Connection, Keypair } from "@solana/web3.js";
import { Wallet } from "@coral-xyz/anchor";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
const FEEDS: Record<string, string> = {
  "SOL": "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
  "BTC": "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
  "ETH": "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
  "BONK": "72b021217ca3fe68922a19aaf990109cb9d84e9ad004b4d2025ad6f529314419",
  "JUP": "0a0408d619e9380abad35060f9192039ed5042fa6f82301d0e48bb52be830996",
  "WIF": "4ca4beeca86f0d164160323817a4e42b10010a724c2217c6ee41b54cd4cc61fc",
  "PYTH": "0bbf28e9a841a1cc788f6a361b17ca072d0ea3098a1e5df1c3922d06719579ff",
  "JTO": "b43660a5f790c69354b0729a5ef9d50d68f1df92107540210b9cccba1f947cc2",
  "DOGE": "dcef50dd0a4cd2dcc17e45df1676dcb336a11a61c69df7a0299b0150c672d25c",
  "HYPE": "4279e31cc369bbcc2faf022b382b080e32a8e689ff20fbc530d2a603eb6cd98b",
  "XAU": "765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2",
  "EUR": "a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b",
  "USDC": "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
  "TRUMP": "879551021853eec7a7dc827578e8e69da7e4fa8148339aa0d3d5296405be4b1a",
  "RAY": "91568baa8beb53db23eb3fb7f22c6e8bd303d103919e19733f2bb642d3e7987a",
};
(async () => {
  const conn = new Connection(process.env.RPC!, "confirmed");
  const r = new PythSolanaReceiver({ connection: conn, wallet: new Wallet(Keypair.generate()) });
  const now = Math.floor(Date.now() / 1000);
  for (const [sym, id] of Object.entries(FEEDS)) {
    const addr = r.getPriceFeedAccountAddress(0, "0x" + id);
    const a = await conn.getAccountInfo(addr);
    if (!a) { console.log(sym.padEnd(6), addr.toBase58(), "MISSING"); continue; }
    const d = a.data; let off = 8 + 32; off += d[off] === 1 ? 1 : 2; off += 32;
    const price = d.readBigInt64LE(off), expo = d.readInt32LE(off + 16), pt = Number(d.readBigInt64LE(off + 20));
    console.log(sym.padEnd(6), addr.toBase58(), "price", Number(price) * 10 ** expo, "age", now - pt, "s");
  }
})();
