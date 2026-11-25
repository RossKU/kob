#!/usr/bin/env node
// Generates the evidence-span check of the stop readers (TPLGEN, audits R10CC-1 and R12-1).
//
// A template's bytes are `0x6b ‖ state ‖ 0x6c ‖ code`: the state span runs as script, and the template hash covers
// only the code around it. A hand-built order can therefore put opcodes where a field should be (R10CC-1), or end its
// span with a push that swallows its whole code, which then never runs (R12-1). A stop reader that takes such an order
// as evidence of a fill would read a fill that never happened. So every reader proves, right after it takes the
// evidence order's span, that each field's push header sits at its canonical offset with its canonical bytes: the
// span then parses push, data, push, data, ..., ends exactly at the span end, and the order's code runs as compiled.
// The values are free.
//
// The header positions are read from the compiled evidence artifacts (their canonical state spans), never written by
// hand. Each reader source gets one function per evidence layout it reads (`spanAsk`, `spanBid`, `spanPair`,
// `spanAskKron`, `spanBidKron`) and, when it reads more than one, `spanBook` (the KAS-book layouts) and `spanAny`
// (all of them), which pick the layout by the span's length (each layout has its own). The functions sit between
// the markers below. A D0 build shares its source and must share its evidence's layout (checked here).
//
//   node scripts/gen-span-check.mjs          rewrite the generated blocks from contracts/artifacts
//   node scripts/gen-span-check.mjs --check  exit 1 if a block differs from what the artifacts give
//
// The positions depend only on the evidence state layouts, not on any code, so one build reaches a fixed point.
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, '$1')), '..');
const check = process.argv.includes('--check');
const BEGIN = '    // BEGIN GENERATED span check (scripts/gen-span-check.mjs): do not edit';
const END = '    // END GENERATED span check';

// The evidence layouts: function name, artifact, and the builds that must share it.
const LAYOUTS = {
  spanAsk: { art: 'KobAsk', same: [] },
  spanBid: { art: 'KobBid', same: [] },
  spanPair: { art: 'KobPair', same: [] },
  spanAskKron: { art: 'KobAskKron', same: ['KobAskKronD0'] },
  spanBidKron: { art: 'KobBidKron', same: ['KobBidKronD0'] },
};
// The reader sources and the evidence layouts they read (book: the KAS-book layouts of `spanBook`).
const KCC = { book: ['spanAsk', 'spanBid'], pair: [] };
const KRON = { book: ['spanAskKron', 'spanBidKron'], pair: [] };
const PAIR = { book: ['spanAsk', 'spanAskKron', 'spanBid', 'spanBidKron'], pair: ['spanPair'] };
const SOURCES = {
  'orders/KobCondAsk.sil': KCC,
  'orders/KobCondBid.sil': KCC,
  'orders/KobIfdAsk.sil': { book: ['spanAsk'], pair: [] },
  'orders/KobIfdBid.sil': { book: ['spanBid'], pair: [] },
  'orders/KobCondPair.sil': PAIR,
  'orders/KobIfdPair.sil': PAIR,
  'orders/kron/KobCondAskKron.sil': KRON,
  'orders/kron/KobCondBidKron.sil': KRON,
  'orders/kron/KobIfdAskKron.sil': { book: ['spanAskKron'], pair: [] },
  'orders/kron/KobIfdBidKron.sil': { book: ['spanBidKron'], pair: [] },
};
// The sources whose evidence fill entry carried the earlier own-span check (R10CC-1): it must be gone.
const FORMER = ['orders/KobAsk.sil', 'orders/KobBid.sil', 'orders/KobPair.sil', 'orders/kron/KobAskKron.sil', 'orders/kron/KobBidKron.sil'];

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

function layout(name) {
  const a = JSON.parse(fs.readFileSync(path.join(root, 'contracts', 'artifacts', `${name}.json`), 'utf8'));
  const contract = a.contracts[name];
  const c = contract.compiled;
  const { offset, len } = c.state_span;
  if (c.bytecode[offset - 1] !== 0x6b || offset !== 1) throw new Error(`${name}: the state span does not follow a lone 0x6b`);
  const hs = headers(c.bytecode.slice(offset, offset + len));
  const fields = contract.runtime_state.fields;
  if (fields.length !== hs.length) throw new Error(`${name}: ${hs.length} pushes for ${fields.length} fields`);
  if (fields.some((f) => f.type.kind.startsWith('dynamic'))) throw new Error(`${name}: a dynamic field in an evidence layout`);
  return { len, hs };
}

const hex = (bytes) => [...bytes].map((x) => x.toString(16).padStart(2, '0')).join('');

function fn(name) {
  const { art, same } = LAYOUTS[name];
  const { len, hs } = layout(art);
  for (const s of same) {
    const o = layout(s);
    if (o.len !== len || JSON.stringify(o.hs.map(([i, h]) => [i, hex(h)])) !== JSON.stringify(hs.map(([i, h]) => [i, hex(h)]))) {
      throw new Error(`${s}: its state layout is not ${art}'s`);
    }
  }
  const slices = hs.map(([o, h]) => `o.slice(${o}, ${o + h.length})`);
  const lines = [];
  for (let k = 0; k < slices.length; k += 4) lines.push(slices.slice(k, k + 4).join(' + '));
  return {
    len,
    text: [
      `    // ${art} evidence: its ${hs.length} push headers at their offsets (${len} state bytes).`,
      `    function ${name}(byte[] o) {`,
      `        require(${lines.join('\n            + ')}`,
      `            == byte[](0x${hs.map(([, h]) => hex(h)).join('')}));`,
      '    }',
    ],
  };
}

function pick(name, set) {
  const fs_ = set.map((n) => [n, fn(n).len]);
  const lens = fs_.map(([, l]) => l);
  if (new Set(lens).size !== lens.length) throw new Error(`${name}: two layouts of one length`);
  // if (l == L1) { f1(o); } else { if (l == L2) { f2(o); } else { ... require(l == Ln); fn(o); } }
  let body = '';
  const last = fs_[fs_.length - 1];
  body = `require(l == ${last[1]}); ${last[0]}(o);`;
  for (let k = fs_.length - 2; k >= 0; k--) body = `if (l == ${fs_[k][1]}) { ${fs_[k][0]}(o); } else { ${body} }`;
  return [`    function ${name}(byte[] o) {`, '        int l = o.length;', `        ${body}`, '    }'];
}

function block(spec) {
  const all = [...spec.book, ...spec.pair];
  const out = [
    BEGIN,
    '    // R10CC-1, R12-1: an evidence order\'s span runs as script and the template hash does not cover it. A reader',
    '    // proves the push headers of the span it takes: then the span parses as data to its end and the order\'s code',
    '    // runs as compiled, so the fill the reader sees is the fill that code enforces. The values are free.',
  ];
  for (const n of all) out.push(...fn(n).text);
  if (spec.book.length > 1) {
    out.push('    // The KAS-book evidence layout of span o, picked by its length.');
    out.push(...pick('spanBook', spec.book));
  }
  if (spec.pair.length) {
    out.push('    // Any evidence layout of span o, picked by its length.');
    out.push(...pick('spanAny', all));
  }
  out.push(END);
  return out.join('\n');
}

let bad = 0;
for (const [src, spec] of Object.entries(SOURCES)) {
  const file = path.join(root, 'contracts', src);
  const text = fs.readFileSync(file, 'utf8');
  const b = text.indexOf(BEGIN);
  const e = text.indexOf(END);
  if (b < 0 || e < b) {
    console.error(`${src}: no generated span-check block`);
    bad++;
    continue;
  }
  const next = text.slice(0, b) + block(spec) + text.slice(e + END.length);
  if (next !== text) {
    if (check) {
      console.error(`${src}: the span check differs from the artifacts`);
      bad++;
    } else {
      fs.writeFileSync(file, next);
      console.log(`${src}: written`);
    }
  }
}
for (const src of FORMER) {
  const text = fs.readFileSync(path.join(root, 'contracts', src), 'utf8');
  if (text.includes(BEGIN) || /\bspanIsData\b/.test(text)) {
    console.error(`${src}: still carries the evidence order's own span check`);
    bad++;
  }
}
process.exit(bad ? 1 : 0);
