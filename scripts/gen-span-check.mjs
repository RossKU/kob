#!/usr/bin/env node
// Generates the state-span check of every order template (TPLGEN, audit R10CC-1).
//
// A template's bytes are `0x6b ‖ state ‖ 0x6c ‖ code`: the state span runs as script, and its fields are read at
// fixed offsets, by the order's own code and by other scripts (stop readers over evidence orders, pair legs). The
// template hash covers only the code around the span, so a hand-built order could put opcodes where a field should
// be. `spanIsData()` requires every field header of the order's own span to be the direct push of its fixed length,
// exactly as the compiler encodes the state; the values are free.
//
// The header positions are read from the compiled artifact itself (its canonical state span), never written by hand.
// The function sits in each source between the markers below, and every non-cancel entry calls it first.
//
//   node scripts/gen-span-check.mjs          rewrite the generated blocks from contracts/artifacts
//   node scripts/gen-span-check.mjs --check  exit 1 if a block differs from what the artifacts give
//
// The positions depend only on the state layout, not on the code, so one build before and one after a change of
// the layout reach a fixed point.
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, '$1')), '..');
const check = process.argv.includes('--check');
const BEGIN = '    // BEGIN GENERATED span check (scripts/gen-span-check.mjs): do not edit';
const END = '    // END GENERATED span check';

// The order sources and the artifact whose span they are checked against (a D0 build shares its source's layout).
const SOURCES = [
  'orders/KobAsk.sil',
  'orders/KobBid.sil',
  'orders/KobCondAsk.sil',
  'orders/KobCondBid.sil',
  'orders/KobCondPair.sil',
  'orders/KobCurveAsk.sil',
  'orders/KobIfdAsk.sil',
  'orders/KobIfdBid.sil',
  'orders/KobIfdPair.sil',
  'orders/KobPair.sil',
  'orders/kron/KobAskKron.sil',
  'orders/kron/KobBidKron.sil',
  'orders/kron/KobCondAskKron.sil',
  'orders/kron/KobCondBidKron.sil',
  'orders/kron/KobIfdAskKron.sil',
  'orders/kron/KobIfdBidKron.sil',
];

/** The headers of a canonical state span: [offset, header bytes] per field. */
export function headers(span) {
  const out = [];
  let i = 0;
  while (i < span.length) {
    const b = span[i];
    let h;
    let n;
    if (b >= 1 && b <= 75) [h, n] = [1, b];
    else if (b === 0x4c) [h, n] = [2, span[i + 1]];
    else if (b === 0x4d) [h, n] = [3, span[i + 1] | (span[i + 2] << 8)];
    else throw new Error(`byte ${i} of the state span is 0x${b.toString(16)}, not a direct push`);
    out.push([i, span.slice(i, i + h)]);
    i += h + n;
  }
  if (i !== span.length) throw new Error(`the pushes end at ${i}, not at the span end ${span.length}`);
  return out;
}

function block(name) {
  const a = JSON.parse(fs.readFileSync(path.join(root, 'contracts', 'artifacts', `${name}.json`), 'utf8'));
  const contract = a.contracts[name];
  const c = contract.compiled;
  const { offset, len } = c.state_span;
  if (c.bytecode[offset - 1] !== 0x6b || offset !== 1) throw new Error(`${name}: the state span does not follow a lone 0x6b`);
  const hs = headers(c.bytecode.slice(offset, offset + len));
  const fields = contract.runtime_state.fields;
  if (fields.length !== hs.length) throw new Error(`${name}: ${hs.length} pushes for ${fields.length} fields`);
  // A dynamic last field (the curve's table) has a length of its own: its push is checked against the redeem
  // script's length, which the entry passes as `rsLen` (checked against the input's P2SH by the order's own code).
  const dynamic = fields[fields.length - 1].type.kind.startsWith('dynamic');
  if (fields.slice(0, -1).some((f) => f.type.kind.startsWith('dynamic'))) throw new Error(`${name}: a dynamic field before the last`);
  const tail = dynamic ? hs.pop() : null;
  const suffix = c.bytecode.length - offset - len;
  // each header is read from the signature script directly: a slice of a fetched span would push the whole span
  // again for every field, and every pushed byte is a script unit
  const slices = hs.map(([o, h]) => `OpTxInputScriptSigSubstr(me, base + ${o}, base + ${o + h.length})`);
  const want = hs.map(([, h]) => h.map((x) => x.toString(16).padStart(2, '0')).join('')).join('');
  const lines = [];
  for (let k = 0; k < slices.length; k += 2) lines.push(slices.slice(k, k + 2).join(' + '));
  const out = [
    BEGIN,
    '    // R10CC-1: the state span runs as script and is read at fixed offsets, by this order and by other scripts.',
    dynamic
      ? `    // It must be data only: each of its ${fields.length} fields a direct push, the first ${hs.length} of their fixed lengths, the last`
      : `    // It must be data only: each of its ${hs.length} fields a direct push of its fixed length (${len} bytes in all),`,
    dynamic
      ? '    // (the table) of every byte left before the code, as the compiler encodes the state. The values are free.'
      : '    // as the compiler encodes the state. The values are free.',
    "    // Every entry but the maker's cancel calls it first.",
    dynamic ? '    function spanIsData(int rsLen) {' : '    function spanIsData() {',
    '        int me = this.activeInputIndex;',
    `        int base = OpTxInputScriptSigLen(me) - ${dynamic ? 'rsLen' : 'this.bytecodeSize'} + 1;`,
    `        byte[] h = ${lines.join('\n            + ')};`,
    `        require(h == byte[](0x${want}));`,
  ];
  if (dynamic) {
    const at = tail[0];
    out.push(
      `        // the table: one push of every byte from state offset ${at} to the code (${suffix} bytes after the state)`,
      `        int tot = rsLen - ${offset + at + suffix};`,
      '        require(tot >= 1);',
      `        byte[] t0 = OpTxInputScriptSigSubstr(me, base + ${at}, base + ${at + 1});`,
      '        if (tot <= 76) {',
      '            require(OpBin2Num(t0 + byte[](0x00)) == tot - 1);',
      '        } else {',
      '            if (tot <= 257) {',
      '                require(t0 == byte[](0x4c));',
      `                require(OpBin2Num(OpTxInputScriptSigSubstr(me, base + ${at + 1}, base + ${at + 2}) + byte[](0x00)) == tot - 2);`,
      '            } else {',
      '                require(t0 == byte[](0x4d));',
      `                require(OpBin2Num(OpTxInputScriptSigSubstr(me, base + ${at + 1}, base + ${at + 3}) + byte[](0x00)) == tot - 3);`,
      '            }',
      '        }',
    );
  }
  out.push('    }', END);
  return out.join('\n');
}

let bad = 0;
for (const src of SOURCES) {
  const file = path.join(root, 'contracts', src);
  const name = path.basename(src, '.sil');
  const text = fs.readFileSync(file, 'utf8');
  const b = text.indexOf(BEGIN);
  const e = text.indexOf(END);
  if (b < 0 || e < b) {
    console.error(`${src}: no generated span-check block`);
    bad++;
    continue;
  }
  const next = text.slice(0, b) + block(name) + text.slice(e + END.length);
  if (next !== text) {
    if (check) {
      console.error(`${src}: the span check differs from the artifact`);
      bad++;
    } else {
      fs.writeFileSync(file, next);
      console.log(`${src}: written`);
    }
  }
}
process.exit(bad ? 1 : 0);
