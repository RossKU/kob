// Generates the DEV key (funds the setup) and stores it in the gitignored .env.
import { randomBytes } from 'node:crypto';
import { existsSync } from 'node:fs';
import kaspa from '../lib/node-kaspa.mjs';
import { ENV_PATH, loadEnv, upsertEnv } from '../lib/env.mjs';

const env = loadEnv();
// simulated-wallet key (used only by test/e2e-devkey.mjs and test/page-mock.mjs; a local key standing in for a wallet)
if (!env.SIM_WALLET_PRIVATE_KEY) {
  const sk = randomBytes(32).toString('hex');
  const p = new kaspa.PrivateKey(sk);
  upsertEnv({ SIM_WALLET_PRIVATE_KEY: sk, SIM_WALLET_ADDRESS: p.toAddress('testnet').toString(), SIM_WALLET_PUBKEY: p.toPublicKey().toXOnlyPublicKey().toString() });
}
if (env.DEV_PRIVATE_KEY && !process.argv.includes('--force')) {
  const pk = new kaspa.PrivateKey(env.DEV_PRIVATE_KEY);
  console.log('DEV key already in .env');
  console.log('address (fund this with faucet/miner):', pk.toAddress('testnet').toString());
  process.exit(0);
}
const sk = randomBytes(32).toString('hex');
const pk = new kaspa.PrivateKey(sk);
const addr = pk.toAddress('testnet').toString();
upsertEnv({
  NODE_WS: env.NODE_WS || process.env.KOB_TN10_WRPC || 'ws://127.0.0.1:18210',
  DEV_PRIVATE_KEY: sk,
  DEV_ADDRESS: addr,
  DEV_PUBKEY: pk.toPublicKey().toXOnlyPublicKey().toString(),
});
console.log('wrote', ENV_PATH, '(gitignored)');
console.log('DEV address (fund this):', addr);
