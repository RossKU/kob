#!/usr/bin/env node
// The cancel-only table's bytes (docs/spec/template-retirement.md): the compiled bytecode of every retired order template, taken
// from the last commit that carried it, written to crates/kob-protocol/data/retired/<Kind>.<hash prefix>.bin.
//
//   node scripts/extract-retired.mjs           write the files (needs the git history of the commits below)
//   node scripts/extract-retired.mjs --check   fail (exit 1) if a file differs from its artifact in git history
//
// kob-protocol (src/retired.rs) embeds the files and recomputes each template hash from them, so the bytes need no history to be
// checked; this script only shows where they came from.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const out = path.join(root, 'crates/kob-protocol/data/retired');
const check = process.argv.includes('--check');

// [commit that last carried the templates, the kinds retired after it]
const GENERATIONS = [
  // retired by acf39cf (a terminal bid fill returns the spare KAS to its maker); 59d9ea30 is the last release carrying them
  ['59d9ea30', ['KobBid', 'KobCondAsk', 'KobCondBid', 'KobIfdBid', 'KobIfdAsk', 'KobCondPair', 'KobIfdPair', 'KobBidKron', 'KobCondAskKron',
    'KobCondBidKron', 'KobIfdBidKron', 'KobIfdAskKron', 'KobBidKronD0', 'KobCondAskKronD0', 'KobCondBidKronD0', 'KobIfdBidKronD0', 'KobIfdAskKronD0']],
  // retired by da35841 (audit 2026-10-29 F1); 60a4d11 is the last commit carrying them (KobBid, KobBidKron and KobBidKronD0 are the
  // templates above)
  ['60a4d11', ['KobCondAsk', 'KobCondBid', 'KobIfdBid', 'KobIfdAsk', 'KobCondPair', 'KobIfdPair', 'KobCondAskKron', 'KobCondBidKron',
    'KobIfdBidKron', 'KobIfdAskKron', 'KobCondAskKronD0', 'KobCondBidKronD0', 'KobIfdBidKronD0', 'KobIfdAskKronD0']],
];

let bad = 0;
const seen = new Set();
for (const [rev, kinds] of GENERATIONS) {
  for (const kind of kinds) {
    const json = execFileSync('git', ['-C', root, 'show', `${rev}:contracts/artifacts/${kind}.json`], { maxBuffer: 1 << 26 }).toString();
    const c = Object.values(JSON.parse(json).contracts)[0];
    const hash = Buffer.from(c.compiled.template_hash).toString('hex');
    const file = path.join(out, `${kind}.${hash.slice(0, 8)}.bin`);
    if (seen.has(file)) continue;
    seen.add(file);
    const bytes = Buffer.from(c.compiled.bytecode);
    const span = c.compiled.state_span;
    console.log(`${rev} ${kind} ${hash} span ${span.offset}+${span.len} cancel ${c.entries.cancel?.dispatch_tag}`);
    if (check) {
      if (!fs.existsSync(file) || !fs.readFileSync(file).equals(bytes)) {
        console.error(`${path.relative(root, file)} differs from ${rev}:contracts/artifacts/${kind}.json`);
        bad++;
      }
    } else {
      fs.writeFileSync(file, bytes);
    }
  }
}
if (check) {
  for (const f of fs.readdirSync(out)) {
    if (!seen.has(path.join(out, f))) {
      console.error(`${f}: no retired template produces it`);
      bad++;
    }
  }
}
process.exit(bad ? 1 : 0);
