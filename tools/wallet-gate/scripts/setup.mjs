// Setup for the wallet gate (run once per wallet under test):
//   node scripts/setup.mjs --wallet-address kaspatest:q...   [--label kasware] [--tokens 4] [--bids 4] [--funds 3]
//   node scripts/setup.mjs --wallet-pubkey <64 hex x-only>   ...
// Needs .env with DEV_PRIVATE_KEY (see scripts/keygen.mjs) funded on TN10. Writes state/setup-<pubkey>.json.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import kaspa from '../lib/node-kaspa.mjs';
import { ROOT } from '../lib/node-kaspa.mjs';
import { loadEnv } from '../lib/env.mjs';
import { connectRpc } from '../lib/txbuild.mjs';
import { pubkeyOfAddress } from '../lib/contracts.mjs';
import { runSetup } from '../lib/setup-core.mjs';

const args = Object.fromEntries(process.argv.slice(2).reduce((acc, a, i, arr) => (a.startsWith('--') ? [...acc, [a.slice(2), arr[i + 1] && !arr[i + 1].startsWith('--') ? arr[i + 1] : 'true']] : acc), []));
const env = loadEnv();
if (!env.DEV_PRIVATE_KEY) throw new Error('no DEV_PRIVATE_KEY in .env - run: npm run keygen');
let walletPubkey = args['wallet-pubkey'] || env.WALLET_PUBKEY;
if (!walletPubkey && (args['wallet-address'] || env.WALLET_ADDRESS)) walletPubkey = pubkeyOfAddress(kaspa, args['wallet-address'] || env.WALLET_ADDRESS);
if (!walletPubkey || !/^[0-9a-f]{64}$/i.test(walletPubkey)) throw new Error('give --wallet-address kaspatest:q... or --wallet-pubkey <64 hex> (Schnorr x-only key of the wallet account)');
walletPubkey = walletPubkey.toLowerCase();

// serialize concurrent setups: they all spend the dev key's UTXOs
import { mkdirSync as _mk, rmSync as _rm } from 'node:fs';
const LOCK = join(ROOT, 'state', '.setup.lock');
_mk(join(ROOT, 'state'), { recursive: true });
for (let i = 0; ; i++) {
  try { _mk(LOCK); break; } catch { if (i > 300) throw new Error('setup lock held too long: ' + LOCK); await new Promise((r) => setTimeout(r, 2000)); }
}
process.on('exit', () => { try { _rm(LOCK, { recursive: true, force: true }); } catch {} });
const rpc = await connectRpc(kaspa, env.NODE_WS);
try {
  const state = await runSetup({
    k: kaspa, rpc, devKey: new kaspa.PrivateKey(env.DEV_PRIVATE_KEY), walletPubkey,
    kcc20Artifact: JSON.parse(readFileSync(join(ROOT, 'artifacts', 'KCC20Ref.json'), 'utf8')),
    bidTemplate: JSON.parse(readFileSync(join(ROOT, 'artifacts', 'BidOrder.template.json'), 'utf8')),
    tokens: Number(args.tokens ?? 4), bids: Number(args.bids ?? 4), fundCount: Number(args.funds ?? 3),
  });
  if (args.label) state.label = args.label;
  mkdirSync(join(ROOT, 'state'), { recursive: true });
  const file = join(ROOT, 'state', `setup-${walletPubkey}.json`);
  writeFileSync(file, JSON.stringify(state, null, 2));
  console.log('wrote', file);
  console.log(`wallet ${state.walletAddress}: ${state.tokens.length} token UTXOs, ${state.bids.length} bids, ${state.funds.length} funding UTXOs`);
} catch (e) {
  console.error('SETUP FAILED:', e?.stack ?? e);
  process.exitCode = 1;
} finally {
  try { await rpc.disconnect(); } catch {}
  process.exit(process.exitCode ?? 0);
}
