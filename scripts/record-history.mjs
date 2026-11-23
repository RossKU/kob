// Every order template hash the deployment records ever listed, and what this build does with it
// (audit 2026-11-23 A3-2): crates/kob-protocol/data/retired/record-history.tsv.
//
//   node scripts/record-history.mjs          rewrite the file from `git log` of contracts/deploy/*/deployment.json
//
// A hash pinned now is `pinned`, one of the cancel-only table (crates/kob-protocol/src/retired.rs) is `cancel-only`. Any
// other keeps the class and reason the file gives it (`never-deployed` or `pre-f1`, written by hand with the evidence);
// a new one is written `unclassified`, which kob_protocol::retired's test refuses until someone classifies it. Needs git
// and the full history. Read-only except for that file.
import { execSync } from 'node:child_process';
import fs from 'node:fs';

const FILE = 'crates/kob-protocol/data/retired/record-history.tsv';
const sh = (c) => execSync(c, { encoding: 'utf8', maxBuffer: 64 << 20, stdio: ['ignore', 'pipe', 'ignore'] });
const retired = new Set([...fs.readFileSync('crates/kob-protocol/src/retired.rs', 'utf8').matchAll(/"([0-9a-f]{64})"/g)].map((m) => m[1]));
const known = new Map();
if (fs.existsSync(FILE)) {
  for (const l of fs.readFileSync(FILE, 'utf8').split('\n')) {
    if (!l || l.startsWith('#')) continue;
    const [hash, , , , cls, reason] = l.split('\t');
    known.set(hash, { cls, reason });
  }
}
const seen = new Map();
const pinned = new Set();
for (const net of ['testnet-10', 'mainnet']) {
  const path = `contracts/deploy/${net}/deployment.json`;
  for (const t of JSON.parse(fs.readFileSync(path, 'utf8')).templates) pinned.add(t.hash);
  // newest first: the last commit seen is the oldest that carries the hash
  for (const c of sh(`git log --format=%h -- ${path}`).trim().split('\n')) {
    let j;
    try { j = JSON.parse(sh(`git show ${c}:${path}`)); } catch { continue; }
    for (const t of j.templates || []) {
      const v = seen.get(t.hash) || { name: t.name, nets: new Set(), first: c, last: c };
      v.first = c;
      v.nets.add(net);
      seen.set(t.hash, v);
    }
  }
}
for (const h of pinned) if (!seen.has(h)) seen.set(h, { name: '?', nets: new Set(), first: 'uncommitted', last: 'uncommitted' });
const rows = [...seen].map(([hash, v]) => {
  const k = known.get(hash);
  const [cls, reason] = pinned.has(hash)
    ? ['pinned', '']
    : retired.has(hash)
      ? ['cancel-only', '']
      : k && k.cls !== 'pinned' && k.cls !== 'cancel-only' ? [k.cls, k.reason] : ['unclassified', ''];
  return [hash, v.name, [...v.nets].sort().join(','), `${v.first}..${v.last}`, cls, reason].join('\t');
});
rows.sort((a, b) => a.split('\t')[1].localeCompare(b.split('\t')[1]) || a.localeCompare(b));
const head = [
  '# Every order template hash a deployment record listed (scripts/record-history.mjs; audit 2026-11-23 A3-2).',
  '# hash, name, networks, commits that carry it (oldest..newest), class, reason. Classes: pinned, cancel-only (the',
  '# table of crates/kob-protocol/src/retired.rs), never-deployed (no deployment served it), pre-f1 (see the reason).',
];
fs.writeFileSync(FILE, [...head, ...rows].join('\n') + '\n');
console.log(`${rows.length} hashes; unclassified: ${rows.filter((r) => r.split('\t')[4] === 'unclassified').length}`);
