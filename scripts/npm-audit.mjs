#!/usr/bin/env node
// Runs npm audit on one package directory with a narrow allow-list (CI job deps).
//
//   node scripts/npm-audit.mjs <dir> [--allow-file <file>] [--allow <GHSA id>]...
//
// It runs `npm audit --package-lock-only --json` in <dir> and fails (exit 1) on any high or
// critical advisory that is not allowed. The allowed ones are printed.
//
// The allow-list is scripts/npm-audit-allow.json. Each entry has an id, the dirs it applies to and
// a reason, like the accepted RustSec advisories in .cargo/deny.toml. --allow adds an id for this
// run.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
let dir = null;
let allowFile = join(repo, 'scripts/npm-audit-allow.json');
const extra = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--allow-file') allowFile = args[++i];
  else if (args[i] === '--allow') extra.push(args[++i]);
  else if (!dir) dir = args[i];
  else {
    console.error(`unexpected argument ${args[i]}`);
    process.exit(2);
  }
}
if (!dir) {
  console.error('usage: node scripts/npm-audit.mjs <dir> [--allow-file <file>] [--allow <GHSA id>]...');
  process.exit(2);
}
const rel = dir.replace(/\\/g, '/').replace(/\/+$/, '');
const list = JSON.parse(readFileSync(allowFile, 'utf8')).allow ?? [];
const allowed = new Map(list.filter((e) => !e.dirs || e.dirs.includes(rel)).map((e) => [e.id, e.reason]));
for (const id of extra) allowed.set(id, '(--allow)');

const r = spawnSync('npm', ['audit', '--package-lock-only', '--json'], { cwd: resolve(repo, dir), encoding: 'utf8', shell: process.platform === 'win32', maxBuffer: 1 << 28 });
let report;
try {
  report = JSON.parse(r.stdout);
} catch {
  console.error(`npm audit in ${dir} did not return JSON (exit ${r.status}):\n${r.stderr || r.stdout}`);
  process.exit(2);
}
if (report.error) {
  console.error(`npm audit in ${dir} failed: ${JSON.stringify(report.error)}`);
  process.exit(2);
}
// The advisories themselves are the object entries of `via`.
// String entries name the package an advisory reaches through.
const advisories = new Map();
for (const [pkg, v] of Object.entries(report.vulnerabilities ?? {})) {
  for (const a of v.via ?? []) {
    if (typeof a !== 'object') continue;
    const id = String(a.url ?? '').split('/').pop() || String(a.source);
    const e = advisories.get(id) ?? { id, severity: a.severity, title: a.title, packages: new Set() };
    e.packages.add(pkg);
    advisories.set(id, e);
  }
}
let fail = 0;
for (const a of [...advisories.values()].sort((x, y) => x.id.localeCompare(y.id))) {
  const where = [...a.packages].join(', ');
  if (a.severity !== 'high' && a.severity !== 'critical') {
    console.log(`below high  ${a.id} (${a.severity}) ${a.title} [${where}]`);
  } else if (allowed.has(a.id)) {
    console.log(`ALLOWED     ${a.id} (${a.severity}) ${a.title} [${where}]: ${allowed.get(a.id)}`);
  } else {
    console.log(`FAIL        ${a.id} (${a.severity}) ${a.title} [${where}]`);
    fail++;
  }
}
for (const id of allowed.keys()) {
  if (!advisories.has(id)) console.log(`unused      ${id}: no longer reported, remove it from the allow-list`);
}
if (fail) {
  console.error(`${dir}: ${fail} high or critical advisor${fail === 1 ? 'y' : 'ies'} not allowed`);
  process.exit(1);
}
console.log(`${dir}: no high or critical advisory outside the allow-list`);
