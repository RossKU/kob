#!/usr/bin/env node
// The wallet-gate kit (tools/wallet-gate) carries its own, older copies of the KCC-20 reference program and of
// BidOrder, and their artifacts; they are not the programs under contracts/. This checks that those artifacts are
// what the pinned silverc produces from the kit's sources, so they cannot change without their source.
//
//   node scripts/check-wallet-gate.mjs <silverc>     exit 1 if an artifact differs, 2 on a usage error
//
// artifacts/KCC20Ref.json     contracts/KCC20Ref.sil compiled as sil/KCC20.sil (the source_path the artifact records)
//                             with contracts/KCC20Ref.ctor.json
// artifacts/BidOrder.template.json
//                             contracts/BidOrder.sil compiled as sil/BidOrder.sil with the sentinel constructor
//                             arguments of scripts/build-templates.mjs; the `_gate` block that script appends is
//                             compared with what it would write
// Artifacts are compared as parsed JSON (formatting and line endings do not matter). Called by
// scripts/build-contracts.sh --check with the compiler it uses.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const gate = path.join(root, 'tools', 'wallet-gate');
const silverc = process.argv[2];
if (!silverc || !fs.existsSync(path.join(gate, 'lib', 'contracts.mjs'))) {
  console.error('usage: node scripts/check-wallet-gate.mjs <silverc> (from a KOB checkout)');
  process.exit(2);
}
const { BID_PARAMS, SENTINEL_MAKER, SENTINEL_TOKCOV, bidCtorArgs } = await import(pathToFileURL(path.join(gate, 'lib', 'contracts.mjs')).href);

const readJson = (f) => JSON.parse(fs.readFileSync(f, 'utf8'));
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'kob-gate-'));
const compile = (src, sourcePath, ctor) => {
  fs.mkdirSync(path.join(tmp, path.dirname(sourcePath)), { recursive: true });
  fs.copyFileSync(src, path.join(tmp, sourcePath));
  const out = path.join(tmp, `${path.basename(sourcePath, '.sil')}.out.json`);
  execFileSync(/[\\/]/.test(silverc) ? path.resolve(silverc) : silverc, [sourcePath, '--constructor-args', ctor, '-o', out], { cwd: tmp, stdio: ['ignore', 'ignore', 'inherit'] });
  return readJson(out);
};
let fail = 0;
const compare = (name, built, committed) => {
  if (JSON.stringify(built) === JSON.stringify(committed)) console.log(`ok      tools/wallet-gate/artifacts/${name}`);
  else {
    console.error(`DIFFERS tools/wallet-gate/artifacts/${name} (rebuild it from tools/wallet-gate/contracts)`);
    fail = 1;
  }
};

try {
  const kccPath = path.join(gate, 'artifacts', 'KCC20Ref.json');
  const kcc = compile(path.join(gate, 'contracts', 'KCC20Ref.sil'), 'sil/KCC20.sil', path.join(gate, 'contracts', 'KCC20Ref.ctor.json'));
  compare('KCC20Ref.json', kcc, readJson(kccPath));

  // As scripts/build-templates.mjs: the sentinel template over the committed KCC-20 artifact.
  const kcc20 = readJson(kccPath).contracts.KCC20;
  const tplHash = Buffer.from(kcc20.compiled.template_hash).toString('hex');
  const ctor = path.join(tmp, 'bid.ctor.json');
  fs.writeFileSync(ctor, JSON.stringify(bidCtorArgs({ maker: SENTINEL_MAKER, tokenCovId: SENTINEL_TOKCOV, tplHash, kcc20 })));
  const bid = compile(path.join(gate, 'contracts', 'BidOrder.sil'), 'sil/BidOrder.sil', ctor);
  bid._gate = {
    note: 'sentinel-compiled; patch maker/tokenCovId',
    params: Object.fromEntries(Object.entries(BID_PARAMS).map(([k, v]) => [k, v.toString()])),
    sentinelMaker: SENTINEL_MAKER,
    sentinelTokenCovId: SENTINEL_TOKCOV,
    tplHash,
  };
  compare('BidOrder.template.json', bid, readJson(path.join(gate, 'artifacts', 'BidOrder.template.json')));
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}
if (fail) process.exit(1);
console.log('the wallet-gate artifacts reproduce');
