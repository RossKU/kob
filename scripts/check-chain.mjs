#!/usr/bin/env node
// The template chain of the order constructors (audit R10CC-3).
//
// A template that embeds another (the conditional kinds embed KobAsk / KobBid, the if-done kinds embed the conditional
// ones, the pair constructors embed KobAskKron / KobBidKron, ...) carries the other's template hash with its prefix and
// suffix lengths as three constructor arguments in its `.ctor.json`. After a template changes, the files that embed it
// must be rebuilt, until nothing changes: build, fix the chain, build again (in practice two or three rounds).
//
// A stale entry is a 32-byte argument that is the hash of an order template some deployment listed
// (crates/kob-protocol/data/retired/record-history.tsv) but not the hash of the current artifact of the same name.
//
//   node scripts/check-chain.mjs          exit 1 if any constructor file embeds a stale template (build-contracts.sh --check)
//   node scripts/check-chain.mjs --fix    replace each stale triple by the current artifact's (then build again)
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/(\w:)/, '$1')), '..');
const fix = process.argv.includes('--fix');

const art = {};
for (const f of fs.readdirSync(path.join(root, 'contracts', 'artifacts'))) {
  if (!f.endsWith('.json')) continue;
  const a = JSON.parse(fs.readFileSync(path.join(root, 'contracts', 'artifacts', f), 'utf8'));
  for (const [name, c] of Object.entries(a.contracts || {})) {
    const comp = c.compiled;
    if (!comp?.template_hash || !comp.state_span) continue;
    const { offset, len } = comp.state_span;
    art[name] = { hash: Buffer.from(comp.template_hash).toString('hex'), pre: offset, suf: comp.bytecode.length - offset - len };
  }
}
const current = new Map(Object.entries(art).map(([n, v]) => [v.hash, n]));
// every hash an order template of a deployment had, with its kind's name
const recorded = new Map();
for (const line of fs.readFileSync(path.join(root, 'crates/kob-protocol/data/retired/record-history.tsv'), 'utf8').split('\n')) {
  if (!line || line.startsWith('#')) continue;
  const [hash, name] = line.split('\t');
  recorded.set(hash, name);
}

function ctors(dir, acc = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) ctors(p, acc);
    else if (e.name.endsWith('.ctor.json')) acc.push(p);
  }
  return acc;
}

let stale = 0;
for (const p of ctors(path.join(root, 'contracts', 'orders')).concat(ctors(path.join(root, 'contracts', 'locks')))) {
  const args = JSON.parse(fs.readFileSync(p, 'utf8'));
  let changed = false;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a.kind !== 'bytes' || a.value.length !== 32) continue;
    const hex = Buffer.from(a.value).toString('hex');
    const name = current.get(hex);
    if (name) {
      // a current template: its lengths must follow it
      const lens = [args[i + 1]?.value, args[i + 2]?.value];
      if (lens[0] !== art[name].pre || lens[1] !== art[name].suf) {
        console.error(`${path.relative(root, p)}: ${name} ${hex.slice(0, 8)} with lengths ${lens}, not ${art[name].pre}, ${art[name].suf}`);
        stale++;
        if (fix) {
          args[i + 1].value = art[name].pre;
          args[i + 2].value = art[name].suf;
          changed = true;
        }
      }
      continue;
    }
    const was = recorded.get(hex);
    if (!was) continue; // not a template hash (an id, a commitment)
    console.error(`${path.relative(root, p)}: ${was} ${hex.slice(0, 8)} is not the current ${was} (${art[was]?.hash.slice(0, 8) ?? 'none'})`);
    stale++;
    if (fix && art[was]) {
      a.value = [...Buffer.from(art[was].hash, 'hex')];
      args[i + 1].value = art[was].pre;
      args[i + 2].value = art[was].suf;
      changed = true;
    }
  }
  if (changed) {
    const text = '[\n' + args.map((x) => '  ' + JSON.stringify(x).replace(/":/g, '": ').replace(/,"/g, ', "').replace(/,(\d)/g, ', $1')).join(',\n') + '\n]\n';
    fs.writeFileSync(p, text);
  }
}
if (stale && !fix) {
  console.error(`${stale} stale chain entr${stale === 1 ? 'y' : 'ies'}: run node scripts/check-chain.mjs --fix, then build again`);
  process.exit(1);
}
