// Downloads the OFFICIAL rusty-kaspa v2.1.0 wasm SDK (GitHub release asset) into vendor/.
// Note: the npm package "kaspa-wasm" is stale (0.13.0, 2023) and has no tx v1 / covenants.
// The x402 SDK and the wallet tests load this copy and hand it payer keys: it is pinned exactly like web/scripts/fetch-sdk.mjs.
import { mkdirSync, writeFileSync, existsSync, rmSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { unzipSync } from 'fflate';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const URL_ = 'https://github.com/kaspanet/rusty-kaspa/releases/download/v2.1.0/kaspa-wasm32-sdk-v2.1.0.zip';
const VENDOR = join(ROOT, 'vendor');
// Supply-chain pin: the sha256 of the official v2.1.0 release zip (the same value as web/scripts/fetch-sdk.mjs). A download that
// does not hash to this value is REFUSED (nothing is unpacked). Updating the SDK means updating both constants in a reviewed
// commit, never accepting whatever the network returned.
export const EXPECTED_SHA256 = 'ba674e109ff5dd8bedc4dc2ee8a5ecdf4b600b1178a541d77888ec58310b6124';
// The tree the pinned zip unpacks to: sha256 over the lines "<sha256 of the file>  <path>\n" of every file under kaspa-node/ and
// kaspa-web/, sorted by path. An already extracted vendor/ is used only when it still hashes to this (a file changed, added or
// removed there is refused, not loaded).
export const EXPECTED_TREE_SHA256 = '81d931353ae0bd090e338be36bcd9d95800d336851532c4bbdba29a08c6ca603';

function treeSha256() {
  const rows = [];
  for (const top of ['kaspa-node', 'kaspa-web']) {
    const dir = join(VENDOR, top);
    if (!existsSync(dir)) continue;
    for (const rel of readdirSync(dir, { recursive: true })) {
      const p = join(dir, String(rel));
      if (!statSync(p).isFile()) continue;
      rows.push([`${top}/${String(rel).replace(/\\/g, '/')}`, createHash('sha256').update(readFileSync(p)).digest('hex')]);
    }
  }
  rows.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  return createHash('sha256').update(rows.map(([p, h]) => `${h}  ${p}\n`).join('')).digest('hex');
}

const force = process.argv.includes('--force');
if (existsSync(join(VENDOR, 'kaspa-node', 'kaspa.js')) && existsSync(join(VENDOR, 'kaspa-web', 'kaspa.js')) && !force) {
  const tree = treeSha256();
  if (tree !== EXPECTED_TREE_SHA256) {
    throw new Error(`vendor/ does not match the pinned SDK (tree sha256 ${tree}, pinned ${EXPECTED_TREE_SHA256}): rerun with --force to refetch and verify`);
  }
  console.log('vendor/ already populated and verified against the pin (use --force to refetch)');
  process.exit(0);
}
console.log('downloading', URL_);
const res = await fetch(URL_, { redirect: 'follow' });
if (!res.ok) throw new Error('download failed: HTTP ' + res.status);
const buf = new Uint8Array(await res.arrayBuffer());
const sha = createHash('sha256').update(buf).digest('hex');
console.log('zip sha256', sha, 'bytes', buf.length);
if (sha !== EXPECTED_SHA256) throw new Error(`SDK zip hash mismatch: got ${sha}, pinned ${EXPECTED_SHA256}. Refusing to unpack.`);
const wanted = {
  'kaspa-wasm32-sdk/nodejs/kaspa/': 'kaspa-node/',
  'kaspa-wasm32-sdk/web/kaspa/': 'kaspa-web/',
};
const files = unzipSync(buf, { filter: (f) => Object.keys(wanted).some((p) => f.name.startsWith(p)) });
rmSync(VENDOR, { recursive: true, force: true });
for (const [name, data] of Object.entries(files)) {
  const prefix = Object.keys(wanted).find((p) => name.startsWith(p));
  if (!prefix || name.endsWith('/')) continue;
  const out = join(VENDOR, wanted[prefix] + name.slice(prefix.length));
  mkdirSync(dirname(out), { recursive: true });
  writeFileSync(out, data);
}
const tree = treeSha256();
if (tree !== EXPECTED_TREE_SHA256) {
  rmSync(VENDOR, { recursive: true, force: true });
  throw new Error(`the unpacked SDK tree hashes to ${tree}, pinned ${EXPECTED_TREE_SHA256}: removed`);
}
writeFileSync(join(VENDOR, 'SDK_SHA256.txt'), sha + '  kaspa-wasm32-sdk-v2.1.0.zip\n');
console.log('vendor/ ready: kaspa-node (CJS, node) and kaspa-web (ESM, browser)');
