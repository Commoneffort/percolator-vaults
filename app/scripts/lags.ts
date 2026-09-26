import { Connection } from "@solana/web3.js";
import * as C from "../src/chain";
(async () => {
  const conn = new Connection(C.RPC_URL, "confirmed");
  const [m, slot] = await Promise.all([C.fetchMarket(conn), conn.getSlot("confirmed")]);
  for (const v of await C.listVaults(conn)) if (v.canonical && v.status === 1) {
    const a = C.decodeAsset(m.data, v.assetIndex);
    console.log(v.feed?.symbol, "lag", slot - Number(a.slotLast), "oi", a.oiLong.toString());
  }
})();
