// Devnet addresses for the serverless functions (mirrors src/devnet.json and src/chain.ts FEEDS).
export const PERCOLATOR = "8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P";
export const VAULT_PROGRAM = "BSync6F8gtJs3Wj4w8L6H3ZtS397w2XoCYGEAJGmeYX";
export const MARKET = "F9zUEE5MZqTLafnFxW2Zp7rKCQ3Wi3Dh1Mvra5eGMq4n";
export const FEED_SYMBOLS: Record<string, string> = {
  ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d: "SOL",
  e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43: "BTC",
  ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace: "ETH",
  "0a0408d619e9380abad35060f9192039ed5042fa6f82301d0e48bb52be830996": "JUP",
  "4ca4beeca86f0d164160323817a4e42b10010a724c2217c6ee41b54cd4cc61fc": "WIF",
  "0bbf28e9a841a1cc788f6a361b17ca072d0ea3098a1e5df1c3922d06719579ff": "PYTH",
  "91568baa8beb53db23eb3fb7f22c6e8bd303d103919e19733f2bb642d3e7987a": "RAY",
};

/** Wallets that seeded demo activity (scripts/seed.ts). The site labels them. */
export const SEED_WALLETS: Record<string, string> = {
  HRKeXCveDknpLQTuc6FiUzGoxabU53USwJEMbguMPnj9: "seed LP",
  BobEW42aDUTzntjiqavc5fRQWt9VnN1JMBG8dcLV4xRQ: "seed trader 1",
  HoNb9L6dM25qHMQWjn7FqX7CvDWN8g97sdb56UqYtPZF: "seed trader 2",
  "6Bosp4sCkYs2Fbzt2PYY4whVMaL5xUhfkUoytZvbnRWa": "seed trader 3",
  "4caXfNghsBer5SVEykPZhqNTq7te7LTQ9MQ8rVLGUYzK": "demo video",
};
