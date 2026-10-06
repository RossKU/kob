// DEV-ONLY (needs silverc v1.0.0): compiles the BidOrder template with sentinel constructor args so
// setup.mjs / the page can patch `maker` and `tokenCovId` (both 32-byte pushes) without silverc.
// usage: node scripts/build-templates.mjs <path-to-silverc>
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ROOT } from '../lib/node-kaspa.mjs';
import { BID_PARAMS, SENTINEL_MAKER, SENTINEL_TOKCOV, bidCtorArgs } from '../lib/contracts.mjs';

const silverc = process.argv[2] || process.env.SILVERC;
if (!silverc) throw new Error('usage: node scripts/build-templates.mjs <silverc>');
const kcc = JSON.parse(readFileSync(join(ROOT, 'artifacts', 'KCC20Ref.json'), 'utf8')).contracts.KCC20;
const tplHash = Buffer.from(kcc.compiled.template_hash).toString('hex');
const dir = mkdtempSync(join(tmpdir(), 'wg-'));
const ctor = join(dir, 'ctor.json');
writeFileSync(ctor, JSON.stringify(bidCtorArgs({ maker: SENTINEL_MAKER, tokenCovId: SENTINEL_TOKCOV, tplHash, kcc20: kcc })));
const out = join(ROOT, 'artifacts', 'BidOrder.template.json');
execFileSync(silverc, [join(ROOT, 'contracts', 'BidOrder.sil'), '--ctor', ctor, '-o', out], { stdio: 'inherit' });
const a = JSON.parse(readFileSync(out, 'utf8'));
a._gate = { note: 'sentinel-compiled; patch maker/tokenCovId', params: Object.fromEntries(Object.entries(BID_PARAMS).map(([k, v]) => [k, v.toString()])), sentinelMaker: SENTINEL_MAKER, sentinelTokenCovId: SENTINEL_TOKCOV, tplHash };
writeFileSync(out, JSON.stringify(a));
console.log('wrote', out, 'bytecode', a.contracts.BidOrder.compiled.bytecode.length, 'B');
