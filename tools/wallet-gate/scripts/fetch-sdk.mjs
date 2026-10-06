// Downloads the OFFICIAL rusty-kaspa v2.1.0 wasm SDK (GitHub release asset) into vendor/.
// Note: the npm package "kaspa-wasm" is stale (0.13.0, 2023) and has no tx v1 / covenants.
import { mkdirSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { unzipSync } from 'fflate';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const URL_ = 'https://github.com/kaspanet/rusty-kaspa/releases/download/v2.1.0/kaspa-wasm32-sdk-v2.1.0.zip';
const VENDOR = join(ROOT, 'vendor');
if (existsSync(join(VENDOR, 'kaspa-node', 'kaspa.js')) && existsSync(join(VENDOR, 'kaspa-web', 'kaspa.js')) && !process.argv.includes('--force')) {
  console.log('vendor/ already populated (use --force to refetch)');
  process.exit(0);
}
console.log('downloading', URL_);
const res = await fetch(URL_, { redirect: 'follow' });
if (!res.ok) throw new Error('download failed: HTTP ' + res.status);
const buf = new Uint8Array(await res.arrayBuffer());
const sha = createHash('sha256').update(buf).digest('hex');
console.log('zip sha256', sha, 'bytes', buf.length);
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
writeFileSync(join(VENDOR, 'SDK_SHA256.txt'), sha + '  kaspa-wasm32-sdk-v2.1.0.zip\n');
console.log('vendor/ ready: kaspa-node (CJS, node) and kaspa-web (ESM, browser)');
