#!/usr/bin/env node
// Writes the Unicode-derived parts of crates/kob-protocol/data/display-chars.json from this Node's
// own Unicode data, and the file again in its one layout. Everything else in the file is kept as
// it is.
//   emoji         the Extended_Pictographic ranges: a joiner sits only between two of them, and a
//                 variation selector only after one
//   nfc_no        the characters that are never left as they are by NFC normalization (NFC_QC=No)
//   nfc_compose   [combining character, the characters it composes with under NFC] pairs (the
//                 Hangul syllables are composed by rule, in code)
// Both readers, kob-protocol and the web, flag a string with an nfc_no character, or a pair that
// composes, as a look-alike: its NFC twin is drawn the same.
//   node scripts/gen-display-chars.mjs            write the file
//   node scripts/gen-display-chars.mjs --check    exit 1 when the file differs
import { readFileSync, writeFileSync } from 'node:fs';

const FILE = new URL('../../crates/kob-protocol/data/display-chars.json', import.meta.url);
const ch = (c) => String.fromCodePoint(c);
const hex = (c) => c.toString(16).toUpperCase().padStart(4, '0');
const isSurrogate = (c) => c >= 0xd800 && c <= 0xdfff;

function ranges(pred) {
  const out = [];
  let start = -1;
  for (let c = 0; c <= 0x110000; c++) {
    const yes = c <= 0x10ffff && !isSurrogate(c) && pred(c);
    if (yes && start < 0) start = c;
    if (!yes && start >= 0) {
      out.push([hex(start), hex(c - 1)]);
      start = -1;
    }
  }
  return out;
}

const HANGUL = (c) => c >= 0xac00 && c <= 0xd7a3;
const JAMO = (c) => c >= 0x1100 && c <= 0x11ff;

export function derive() {
  const emoji = ranges((c) => /\p{Extended_Pictographic}/u.test(ch(c)));
  const nfcNo = ranges((c) => ch(c).normalize('NFC') !== ch(c));
  // the last character of every canonical decomposition is a character that composes
  const seconds = new Set();
  for (let c = 0; c <= 0x10ffff; c++) {
    if (isSurrogate(c) || HANGUL(c)) continue;
    const d = [...ch(c).normalize('NFD')];
    if (d.length > 1) seconds.add(d[d.length - 1].codePointAt(0));
  }
  const compose = [];
  for (const m of [...seconds].sort((a, b) => a - b)) {
    const bases = [];
    for (let a = 0; a <= 0x10ffff; a++) {
      if (isSurrogate(a) || JAMO(a) || HANGUL(a)) continue;
      if ([...(ch(a) + ch(m)).normalize('NFC')].length === 1) bases.push(a);
    }
    if (bases.length) compose.push([hex(m), bases.map(ch).join('')]);
  }
  return { emoji, nfc_no: nfcNo, nfc_compose: compose };
}

// ---- the file's layout
const esc = (s) => JSON.stringify(s).replace(/[\u007f-￿]/g, (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`);
const rangeList = (r, perLine) => {
  const lines = [];
  for (let i = 0; i < r.length; i += perLine) lines.push('    ' + r.slice(i, i + perLine).map(([a, b]) => `[${esc(a)},${esc(b)}]`).join(','));
  return `[\n${lines.join(',\n')}\n  ]`;
};

export function render(d) {
  const parts = [];
  for (const [k, v] of Object.entries(d)) {
    let text;
    if (k === 'refuse' || k === 'emoji' || k === 'nfc_no' || k === 'lookalike_ranges') text = rangeList(v, k === 'refuse' ? 1 : 6);
    else if (k === 'nfc_compose') text = `[\n${v.map(([m, b]) => `    [${esc(m)},${esc(b)}]`).join(',\n')}\n  ]`;
    else if (k === 'cases') text = `[\n${v.map((c) => `    {"s":${esc(c.s)},"bad":${c.bad},"lookalike":${c.lookalike}}`).join(',\n')}\n  ]`;
    else if (Array.isArray(v)) text = `[${v.map(esc).join(',')}]`;
    else text = esc(v);
    parts.push(`  ${esc(k)}: ${text}`);
  }
  return `{\n${parts.join(',\n')}\n}\n`;
}

{
  const before = readFileSync(FILE, 'utf8');
  const doc = JSON.parse(before);
  const gen = derive();
  // the generated keys go after the rules they serve, in a fixed order
  const out = {};
  for (const [k, v] of Object.entries(doc)) {
    if (k in gen) continue;
    out[k] = v;
    if (k === 'tag_end') out.emoji = gen.emoji;
    if (k === 'lookalike_chars') {
      out.nfc_no = gen.nfc_no;
      out.nfc_compose = gen.nfc_compose;
    }
  }
  const text = render(out);
  if (process.argv.includes('--check')) {
    if (text !== before) {
      console.error('display-chars.json differs from what this Node (Unicode ' + process.versions.unicode + ') derives: run node scripts/gen-display-chars.mjs');
      process.exit(1);
    }
  } else {
    writeFileSync(FILE, text);
    console.log(`display-chars.json written (Unicode ${process.versions.unicode})`);
  }
}
