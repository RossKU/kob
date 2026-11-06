#!/usr/bin/env node
// Builds the frozen compression dictionaries of crates/kobd/src/indexer/packed_dicts.rs from a KOB indexer database.
//
//   node scripts/build-state-dicts.mjs <index.sqlite3> [--out crates/kobd/src/indexer/packed_dicts.rs] [--first-id N]
//
// What they are for (indexer/packed.rs): the genesis state of an order (`orders.genesis_state`, about 380 bytes) and the detail JSON of
// an event (`order_events.detail`, about 110 bytes) are stored deflated against a dictionary: a representative of the same kind, then,
// for a genesis state, the order's own maker, token, token program and quote token (which its state repeats). A dictionary is part of the
// stored format: a value names its dictionary by id and can be read for as long as the build carries that id. THE GENERATED FILE IS
// APPEND-ONLY: a better dictionary gets a new id (run this with --first-id above the last id in the file and add its entries to
// the file by hand), an existing entry is never edited or removed (the test `dictionaries_are_frozen` pins their hashes).
//
// The representative of each contract / kind is the one (out of a sample of its rows) that deflates the other sampled rows to the fewest
// bytes; its maker, token, program, quote-token and extension bytes are zeroed (the row's own are appended at pack time), so no key or
// token id of the database it was chosen from is kept.
import { DatabaseSync } from 'node:sqlite';
import { createHash } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import zlib from 'node:zlib';

const args = process.argv.slice(2);
const dbPath = args.find((a) => !a.startsWith('--'));
const opt = (k, d) => { const i = args.indexOf(`--${k}`); return i >= 0 ? args[i + 1] : d; };
if (!dbPath) { console.error('usage: build-state-dicts.mjs <index.sqlite3> [--out file] [--first-id N]'); process.exit(2); }
const out = opt('out', 'crates/kobd/src/indexer/packed_dicts.rs');
let nextId = Number(opt('first-id', '1'));
const db = new DatabaseSync(dbPath, { readOnly: true });
const buf = (x) => (x == null ? Buffer.alloc(0) : Buffer.from(x));
const size = (data, dict) => zlib.deflateRawSync(data, { level: 9, dictionary: dict }).length;
const hex = (b) => Buffer.from(b).toString('hex');

function scrub(state, fields) {
  const s = Buffer.from(state);
  for (const f of fields) {
    if (f.length < 8) continue;
    for (let at = s.indexOf(f); at >= 0; at = s.indexOf(f, at + f.length)) s.fill(0, at, at + f.length);
  }
  return s;
}

// ---- genesis states, per contract
const contracts = db.prepare('SELECT contract, COUNT(*) n FROM orders GROUP BY contract ORDER BY contract').all();
const dicts = [];
for (const { contract, n } of contracts) {
  const step = Math.max(1, Math.floor(Number(n) / 400));
  const rows = db.prepare(`SELECT genesis_state g, maker, token_cov_id t, token_tpl_hash p, quote_cov_id q, ext_commit e FROM orders WHERE contract = ? AND rowid % ${step} = 0 LIMIT 400`).all(contract);
  if (rows.length < 3) { console.error(`skip ${contract}: ${rows.length} rows`); continue; }
  const extras = (r) => Buffer.concat([buf(r.maker), buf(r.t), buf(r.p), buf(r.q)]);
  const cands = rows.filter((_, i) => i % Math.max(1, Math.floor(rows.length / 24)) === 0);
  let best = null;
  for (const c of cands) {
    const skel = scrub(c.g, [buf(c.maker), buf(c.t), buf(c.p), buf(c.q), buf(c.e)]);
    let total = 0;
    for (const r of rows) total += size(Buffer.from(r.g), Buffer.concat([skel, extras(r)]));
    if (!best || total < best.total) best = { skel, total };
  }
  const raw = rows.reduce((s, r) => s + r.g.length, 0);
  console.log(`genesis ${contract.padEnd(16)} ${String(n).padStart(8)} orders  ${(raw / rows.length).toFixed(0)} -> ${(best.total / rows.length).toFixed(1)} bytes  (dictionary ${best.skel.length} bytes)`);
  dicts.push({ kind: 'genesis', name: contract, bytes: best.skel, avg: best.total / rows.length });
}

// ---- event details, per kind
for (const { kind, n } of db.prepare('SELECT kind, COUNT(*) n FROM order_events WHERE detail IS NOT NULL GROUP BY kind ORDER BY kind').all()) {
  const step = Math.max(1, Math.floor(Number(n) / 600));
  const rows = db.prepare(`SELECT detail d FROM order_events WHERE kind = ? AND detail IS NOT NULL AND typeof(detail) = 'text' AND rowid % ${step} = 0 LIMIT 600`).all(kind).map((r) => Buffer.from(r.d));
  if (rows.length < 3) { console.error(`skip ${kind}: ${rows.length} rows`); continue; }
  const cands = rows.filter((_, i) => i % Math.max(1, Math.floor(rows.length / 40)) === 0);
  let best = null;
  for (const c of cands) {
    let total = 0;
    for (const r of rows) total += size(r, c);
    if (!best || total < best.total) best = { dict: c, total };
  }
  // a second representative in front when it pays (shapes that differ in their keys)
  let chosen = best.dict, total = best.total;
  for (const c of cands) {
    const d = Buffer.concat([c, best.dict]);
    let t = 0;
    for (const r of rows) t += size(r, d);
    if (t < total * 0.93) { chosen = d; total = t; }
  }
  const raw = rows.reduce((s, r) => s + r.length, 0);
  console.log(`detail  ${kind.padEnd(16)} ${String(n).padStart(8)} events  ${(raw / rows.length).toFixed(0)} -> ${(total / rows.length).toFixed(1)} bytes  (dictionary ${chosen.length} bytes)`);
  dicts.push({ kind: 'detail', name: kind, bytes: chosen, avg: total / rows.length });
}

let src = `//! GENERATED by scripts/build-state-dicts.mjs from a KOB indexer database. FROZEN AND APPEND-ONLY: a stored value names its dictionary
//! by id (\`indexer::packed\`), so an entry is never edited or removed; a better dictionary is a new id.
//! Genesis dictionaries are a representative state of the contract with its maker, token, program, quote token and extension bytes zeroed;
//! detail dictionaries are one or two representative JSON strings of the event kind.

/// (id, what it is for: \`genesis\` or \`detail\`, the contract or event kind, the bytes in hex)
#[rustfmt::skip]
pub const DICTIONARIES: &[(u8, &str, &str, &str)] = &[
`;
for (const d of dicts) {
  const id = nextId++;
  src += `    (${id}, "${d.kind}", "${d.name}", "${hex(d.bytes)}"),\n`;
  d.id = id;
}
src += `];\n\n/// SHA-256 of the dictionaries in id order (id, kind, name, bytes, each length-prefixed): the test \`dictionaries_are_frozen\` compares it.\npub const FROZEN_SHA256: &str = "${(() => {
  const h = createHash('sha256');
  for (const d of dicts) {
    for (const part of [Buffer.from([d.id]), Buffer.from(d.kind), Buffer.from(d.name), d.bytes]) {
      const len = Buffer.alloc(4); len.writeUInt32LE(part.length); h.update(len); h.update(part);
    }
  }
  return h.digest('hex');
})()}";\n`;
writeFileSync(out, src);
console.log(`wrote ${out}: ${dicts.length} dictionaries, ${dicts.reduce((s, d) => s + d.bytes.length, 0)} bytes`);
