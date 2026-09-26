// A devnet-only wallet that lives in this browser tab: a keypair kept in localStorage that signs
// without popups. It lets anyone (and judges) try the app without installing a wallet. Never use
// it for real funds: anyone with access to this browser profile can read the key.
import {
  BaseSignerWalletAdapter,
  WalletName,
  WalletNotConnectedError,
  WalletReadyState,
} from "@solana/wallet-adapter-base";
import { Keypair, PublicKey, Transaction, VersionedTransaction } from "@solana/web3.js";

const STORAGE_KEY = "percolator-vaults:devnet-burner";
export const BurnerName = "Devnet Burner" as WalletName<"Devnet Burner">;

const ICON =
  "data:image/svg+xml;base64," +
  btoa(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#5b7cf0"/><path d="M16 5c2 5 7 7 7 13a7 7 0 0 1-14 0c0-3 2-5 3-7 1 2 2 3 3 3-1-4 0-7 1-9z" fill="#fff"/></svg>`,
  );

function loadKeypair(): Keypair {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(raw)));
  } catch { /* storage unavailable: fall through to a fresh key for this tab */ }
  const kp = Keypair.generate();
  try { localStorage.setItem(STORAGE_KEY, JSON.stringify(Array.from(kp.secretKey))); } catch { /* ignore */ }
  return kp;
}

export class DevnetBurnerAdapter extends BaseSignerWalletAdapter {
  name = BurnerName;
  url = "https://percolator-vaults.vercel.app";
  icon = ICON;
  supportedTransactionVersions = new Set(["legacy", 0] as const);
  private keypair: Keypair | null = null;

  get connecting() { return false; }
  get publicKey(): PublicKey | null { return this.keypair?.publicKey ?? null; }
  get readyState() { return WalletReadyState.Loadable; }

  async connect() {
    this.keypair = loadKeypair();
    this.emit("connect", this.keypair.publicKey);
  }

  async disconnect() {
    this.keypair = null;
    this.emit("disconnect");
  }

  async signTransaction<T extends Transaction | VersionedTransaction>(tx: T): Promise<T> {
    if (!this.keypair) throw new WalletNotConnectedError();
    if (tx instanceof VersionedTransaction) tx.sign([this.keypair]);
    else tx.partialSign(this.keypair);
    return tx;
  }

  async signAllTransactions<T extends Transaction | VersionedTransaction>(txs: T[]): Promise<T[]> {
    return Promise.all(txs.map(t => this.signTransaction(t)));
  }
}
