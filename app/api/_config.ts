// Devnet addresses for the serverless functions (mirrors src/devnet.json and src/chain.ts FEEDS).
export const PERCOLATOR = "8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P";
export const VAULT_PROGRAM = "BSync6F8gtJs3Wj4w8L6H3ZtS397w2XoCYGEAJGmeYX";
export const MARKET = "8ZMjDzhAgcV7zidvkRXM257Dp2HmzeNz7jpbTFvRE1Ls";
export const ROUTER = "DkK9TSMpVXLq26HeqxTXLysXyRKDYHTKU94SLFDWgjw3";
export const FEED_SYMBOLS: Record<string, string> = {
  ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d: "SOL",
  e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43: "BTC",
  ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace: "ETH",
  "0bbf28e9a841a1cc788f6a361b17ca072d0ea3098a1e5df1c3922d06719579ff": "PYTH",
  dcef50dd0a4cd2dcc17e45df1676dcb336a11a61c69df7a0299b0150c672d25c: "DOGE",
  "4279e31cc369bbcc2faf022b382b080e32a8e689ff20fbc530d2a603eb6cd98b": "HYPE",
  "765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2": "XAU",
  a995d00bb36a63cef7fd2c287dc105fc8f3d93779f062f09551b0af3e81ec30b: "EUR",
};

/** Wallets that seeded demo activity (scripts/seed.ts). The site labels them. */
export const SEED_WALLETS: Record<string, string> = {
  HRKeXCveDknpLQTuc6FiUzGoxabU53USwJEMbguMPnj9: "seed LP",
  BobEW42aDUTzntjiqavc5fRQWt9VnN1JMBG8dcLV4xRQ: "seed trader 1",
  HoNb9L6dM25qHMQWjn7FqX7CvDWN8g97sdb56UqYtPZF: "seed trader 2",
  "6Bosp4sCkYs2Fbzt2PYY4whVMaL5xUhfkUoytZvbnRWa": "seed trader 3",
  "4caXfNghsBer5SVEykPZhqNTq7te7LTQ9MQ8rVLGUYzK": "demo video",
};
