// Generates the soak's keys (bank, market maker, traders, x402 payer and merchant, two executor hot keys) into run/keys.json
// (gitignored), the executors' 64-hex key files run/exec-{a,b}/operator.key and the x402 merchant API key. Idempotent.
import { createHash, randomBytes } from 'node:crypto';
import { createRequire } from 'node:module';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const RUN = process.env.SOAK_RUN_DIR ?? join(HERE, '..', 'run');
const require = createRequire(import.meta.url);
const kaspa = require(join(HERE, '..', '..', '..', 'web', 'vendor', 'kaspa-node', 'kaspa.js'));

const NAMES = ['bank', 'mm', 't1', 't2', 't3', 't4', 't5', 'payer', 'merchant', 'execA', 'execB'];
mkdirSync(RUN, { recursive: true });
const path = join(RUN, 'keys.json');
const keys = existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : {};
for (const n of NAMES) {
  if (keys[n]) continue;
  const sk = randomBytes(32).toString('hex');
  const p = new kaspa.PrivateKey(sk);
  keys[n] = { secretKey: sk, publicKey: p.toPublicKey().toXOnlyPublicKey().toString(), address: p.toAddress('testnet-10').toString() };
}
writeFileSync(path, JSON.stringify(keys, null, 2) + '\n', { mode: 0o600 });
for (const [n, dir] of [['execA', 'exec-a'], ['execB', 'exec-b']]) {
  mkdirSync(join(RUN, dir), { recursive: true });
  writeFileSync(join(RUN, dir, 'operator.key'), keys[n].secretKey + '\n', { mode: 0o600 });
}
// the x402 merchant's facilitator API key (the facilitator config stores only its sha256)
if (!existsSync(join(RUN, 'x402-merchant.key'))) {
  const apiKey = 'soak-' + randomBytes(24).toString('hex');
  writeFileSync(join(RUN, 'x402-merchant.key'), apiKey + '\n', { mode: 0o600 });
  writeFileSync(join(RUN, 'x402-merchant.sha256'), createHash('sha256').update(apiKey).digest('hex') + '\n');
}
for (const n of NAMES) console.log(n.padEnd(9), keys[n].address);
