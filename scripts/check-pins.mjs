#!/usr/bin/env node
// Every 64-hex constant in the files that pin build outputs must be a hash the generated artifacts carry.
//
//   node scripts/check-pins.mjs [repo root]      exit 1 if a constant matches no generated artifact
//
// Known hashes: every 64-hex string and every 32-byte array in contracts/artifacts, contracts/argent,
// contracts/deploy and contracts/third-party (the outputs scripts/build-contracts.sh and scripts/build-deploy.sh
// reproduce, plus the vendored third-party artifacts they list). Pin files: the Rust pin tables, the TS SDK
// constants, the router generator head, the Argent examples, the registry's template hashes and the docs. In
// registry/tokens.json only the values of `*hash*` keys are build outputs; transaction ids and the other
// chain data there are not, and are skipped. Run it after scripts/build-contracts.sh --check (CI does).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(process.argv[2] ?? path.join(path.dirname(fileURLToPath(import.meta.url)), '..'));
if (!fs.existsSync(path.join(root, 'contracts/SHA256SUMS'))) {
  console.error(`check-pins: ${root} is not the KOB repository root`);
  process.exit(2);
}
const rel = (p) => path.relative(root, p).split(path.sep).join('/');
const walk = (d) =>
  fs.existsSync(d)
    ? fs.readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(d, e.name)) : [path.join(d, e.name)]))
    : [];
const HEX64 = /(?<![0-9a-fA-F])[0-9a-f]{64}(?![0-9a-fA-F])/g;

// 1. Hashes carried by the generated artifacts.
const known = new Map();
const add = (h, src) => known.has(h) || known.set(h, src);
const collect = (v, src) => {
  if (Array.isArray(v)) {
    if (v.length === 32 && v.every((x) => Number.isInteger(x) && x >= 0 && x < 256)) add(Buffer.from(v).toString('hex'), src);
    v.forEach((x) => collect(x, src));
  } else if (v && typeof v === 'object') {
    Object.values(v).forEach((x) => collect(x, src));
  } else if (typeof v === 'string') {
    for (const m of v.matchAll(HEX64)) add(m[0], src);
  }
};
const outputs = ['contracts/artifacts', 'contracts/argent', 'contracts/deploy', 'contracts/third-party'].flatMap((d) => walk(path.join(root, d)));
for (const f of outputs) {
  if (f.endsWith('.bin')) continue;
  const text = fs.readFileSync(f, 'utf8');
  if (f.endsWith('.json')) collect(JSON.parse(text), rel(f));
  else for (const m of text.matchAll(HEX64)) add(m[0], rel(f));
}

// 2. Constants in the pin files.
const pinFiles = [
  'crates/kob-protocol/src/artifacts.rs',
  'crates/kob-protocol/src/router.rs',
  'packages/kob-x402/src/types.ts',
  'tools/router-gen/router_head.ag',
  'README.md',
  'contracts/README.md',
  ...walk(path.join(root, 'contracts/argent/examples')).map(rel),
  ...walk(path.join(root, 'docs')).filter((f) => f.endsWith('.md')).map(rel),
];
let checked = 0;
const missing = [];
const check = (h, where) => {
  checked++;
  if (!known.has(h)) missing.push(`${where} ${h}`);
};
for (const f of [...new Set(pinFiles)]) {
  const abs = path.join(root, f);
  if (!fs.existsSync(abs)) {
    missing.push(`${f}: pin file is missing`);
    continue;
  }
  fs.readFileSync(abs, 'utf8')
    .split('\n')
    .forEach((line, i) => {
      for (const m of line.matchAll(HEX64)) check(m[0], `${f}:${i + 1}`);
    });
}
const registry = JSON.parse(fs.readFileSync(path.join(root, 'registry/tokens.json'), 'utf8'));
const visitRegistry = (v, at) => {
  if (Array.isArray(v)) v.forEach((x, i) => visitRegistry(x, `${at}[${i}]`));
  else if (v && typeof v === 'object') {
    for (const [k, x] of Object.entries(v)) {
      if (/hash/i.test(k) && typeof x === 'string') for (const m of x.matchAll(HEX64)) check(m[0], `registry/tokens.json ${at}.${k}`);
      else visitRegistry(x, `${at}.${k}`);
    }
  }
};
visitRegistry(registry, '$');

console.log(`${checked} pinned hashes checked against ${known.size} hashes of ${outputs.length} generated files`);
if (missing.length) {
  console.error(`${missing.length} pinned value(s) match no generated artifact (stale pin, or an artifact that is not reproduced):`);
  for (const m of missing) console.error(`  ${m}`);
  process.exit(1);
}
console.log('every pin matches a generated artifact');
