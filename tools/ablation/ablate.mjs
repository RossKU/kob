#!/usr/bin/env node
// Contract ablation (mutation) runner for the four v2 harness suites. See README.md.
//
//   node tools/ablation/ablate.mjs [--suite kcc20-sell|kcc20-buy|kron-sell|kron-buy|kcc20-cross|kron-cross|all] [--only ID,suite:ID]
//        [--jobs N] [--exe-dir DIR] [--out DIR] [--dry-run] [--baseline] [--no-baseline] [--list] [--keep]
//        [--release] [--timeout SEC]
//
// For every mutation: copy the committed contracts of the suite into a scratch directory, apply the edits (each must match
// exactly once and change the text), run the named test function of the prebuilt test executable with KOB_ABLATION=1 and
// KOB_ABLATION_SRC=<scratch>, and check that the named attack scenarios flipped from rejected to accepted.
import fs from 'node:fs';
import path from 'node:path';
import cp from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { PENDING_SUITES, SUITE_NAMES, loadSuite } from './lib/suites.mjs';
import { applyEdits, EditError } from './lib/edits.mjs';
import { parseOutput } from './lib/parse.mjs';
import { judgeMutation, judgeBaseline } from './lib/judge.mjs';
import { findInDir, findViaCargo } from './lib/exe.mjs';
import { writeReport, totals, totalsLine } from './lib/report.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..');

// ---------------------------------------------------------------- CLI
const VALUE_FLAGS = new Set(['--suite', '--only', '--jobs', '--exe-dir', '--out', '--timeout']);
const BOOL_FLAGS = new Set(['--dry-run', '--baseline', '--no-baseline', '--list', '--keep', '--release', '--help', '-h']);
const camel = a => a.replace(/^-+/, '').replace(/-(\w)/g, (_, c) => c.toUpperCase());
function parseArgs(argv) {
  const o = { suite: 'all', jobs: 4, timeout: 1800 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (VALUE_FLAGS.has(a)) {
      if (i + 1 >= argv.length) die(`${a} needs a value`);
      o[camel(a)] = argv[++i];
    } else if (BOOL_FLAGS.has(a)) o[a === '-h' ? 'help' : camel(a)] = true;
    else die(`unknown argument ${a} (try --help)`);
  }
  o.jobs = Number(o.jobs);
  o.timeout = Number(o.timeout);
  if (!Number.isInteger(o.jobs) || o.jobs < 1) die('--jobs must be a positive integer');
  if (!(o.timeout > 0)) die('--timeout must be a positive number of seconds');
  return o;
}
function die(msg) {
  console.error(`ablate: ${msg}`);
  process.exit(2);
}
const HELP = `usage: node tools/ablation/ablate.mjs [options]
  --suite NAME[,NAME]  kcc20-sell | kcc20-buy | kron-sell | kron-buy | kcc20-cross | kron-cross | all (default all)
  --only ID[,ID]       run only these mutations (id, or suite:id when ids collide across suites)
  --jobs N             parallel test processes (default 4)
  --exe-dir DIR        use prebuilt <bin>-<hash>[.exe] from DIR (newest) instead of asking cargo
  --out DIR            scratch + results directory (default <repo>/target/ablation)
  --dry-run            only apply and validate the edits, run nothing
  --baseline           only run each (suite, test fn) once without mutation; require zero ABLATION-PASS / POS-FAIL
  --no-baseline        skip the automatic baseline run in front of the mutations
  --list               print the mutations and exit
  --keep               keep the scratch directories of confirmed mutations
  --release            with cargo: build the test executables in release mode
  --timeout SEC        kill a test run after SEC seconds (default 1800)`;

// ---------------------------------------------------------------- helpers
const now = () => Date.now();
const secs = t0 => Number(((now() - t0) / 1000).toFixed(1));
const readSil = p => fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n');

function copySources(suite, dir) {
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  for (const f of suite.silFiles) fs.writeFileSync(path.join(dir, f), readSil(path.join(suite.srcAbs, f)));
}

function prepareMutation(suite, mut, outDir) {
  const dir = path.join(outDir, suite.name, mut.id);
  copySources(suite, dir);
  const log = [];
  try {
    for (const t of [{ file: mut.file, edits: mut.edits }, ...mut.also]) {
      const p = path.join(dir, `${t.file}.sil`);
      const r = applyEdits(fs.readFileSync(p, 'utf8'), t.edits);
      fs.writeFileSync(p, r.text);
      for (const l of r.log) log.push(`${t.file}: ${l}`);
    }
  } catch (e) {
    if (!(e instanceof EditError)) throw e;
    log.push(`EDIT-ERROR ${e.message}`);
    fs.writeFileSync(path.join(dir, 'edits.txt'), log.join('\n') + '\n');
    return { dir, error: e.message };
  }
  fs.writeFileSync(path.join(dir, 'edits.txt'), log.join('\n') + '\n');
  return { dir };
}

// A test reference is `<fn>` (a test fn of the suite's binary) or `<bin>::<fn>` (a test fn of another kob-tests binary).
const binOf = (suite, ref) => (ref.includes('::') ? ref.split('::')[0] : suite.testBin);
const fnOf = ref => (ref.includes('::') ? ref.split('::').slice(1).join('::') : ref);
const fileOf = ref => ref.replace(/::/g, '__');

function runTest(exe, ref, srcDir, timeoutSec) {
  const testFn = fnOf(ref);
  return new Promise(resolve => {
    const t0 = now();
    const p = cp.spawn(exe, ['--exact', testFn, '--nocapture', '--test-threads=1'], {
      cwd: ROOT,
      env: { ...process.env, KOB_ABLATION: '1', KOB_ABLATION_SRC: srcDir },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    let timedOut = false;
    p.stdout.on('data', d => (stdout += d));
    p.stderr.on('data', d => (stderr += d));
    const timer = setTimeout(() => {
      timedOut = true;
      p.kill();
    }, timeoutSec * 1000);
    p.on('error', e => {
      clearTimeout(timer);
      resolve({ code: null, signal: String(e), stdout, stderr, timedOut, secs: secs(t0) });
    });
    p.on('close', (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal, stdout, stderr, timedOut, secs: secs(t0) });
    });
  });
}

async function pool(tasks, n) {
  let next = 0;
  const workers = Array.from({ length: Math.min(n, tasks.length) }, async () => {
    while (next < tasks.length) await tasks[next++]();
  });
  await Promise.all(workers);
}

function writeLog(dir, testFn, r) {
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, `${fileOf(testFn)}.log`), `${r.stdout}\n--- stderr ---\n${r.stderr}\n`);
}

// ---------------------------------------------------------------- main
const opt = parseArgs(process.argv.slice(2));
if (opt.help) {
  console.log(HELP);
  process.exit(0);
}
const OUT = path.resolve(opt.out ?? path.join(ROOT, 'target', 'ablation'));

const wanted = opt.suite === 'all' ? SUITE_NAMES.filter(s => !PENDING_SUITES[s]) : opt.suite.split(',').map(s => s.trim());
for (const w of wanted) if (!SUITE_NAMES.includes(w)) die(`unknown suite ${w} (have ${SUITE_NAMES.join(', ')})`);
for (const w of wanted) if (PENDING_SUITES[w]) die(`suite ${w}: ${PENDING_SUITES[w]}`);
if (opt.suite === 'all') for (const [s, why] of Object.entries(PENDING_SUITES)) console.log(`skipping ${s} (${why})`);
const suites = [];
for (const name of wanted) suites.push(await loadSuite(name, ROOT));

if (opt.only) {
  const toks = opt.only.split(',').map(s => s.trim()).filter(Boolean);
  const matched = new Set();
  for (const s of suites) {
    s.mutations = s.mutations.filter(m => {
      const hit = toks.filter(t => t === m.id || t === m.key);
      hit.forEach(t => matched.add(t));
      return hit.length > 0;
    });
  }
  for (const t of toks) if (!matched.has(t)) die(`--only ${t}: no such mutation in the selected suites`);
}

if (opt.list) {
  for (const s of suites) {
    console.log(`# ${s.suite.name}  (${s.suite.family}; ${s.suite.testBin}; ${s.suite.srcDir}; ${s.mutations.length} mutations)`);
    for (const m of s.mutations) {
      const runs = m.runs.map(r => `${r.testFn}: ${[...r.expect, ...r.hold.map(h => '!' + h)].join(',')}`).join(' | ');
      const extra = m.also.length ? ` +also ${m.also.map(a => a.file).join(',')}` : '';
      console.log(`${m.key.padEnd(18)} ${m.file}${extra}  ${m.holdOnly ? '[defence in depth, expected NOT to flip] ' : ''}${runs}  -- ${m.note}`);
    }
  }
  process.exit(0);
}

const T0 = now();
const results = suites.map(s => ({
  name: s.suite.name,
  family: s.suite.family,
  testBin: s.suite.testBin,
  srcDir: s.suite.srcDir,
  templates: [],
  baseline: [],
  mutations: [],
}));
const byName = Object.fromEntries(suites.map((s, i) => [s.suite.name, { s, r: results[i] }]));

// ---- (1) apply the edits
const prepared = []; // { s, r, m, dir, res }
for (const { s, r } of suites.map(s => byName[s.suite.name])) {
  for (const m of s.mutations) {
    const p = prepareMutation(s.suite, m, OUT);
    const rec = { id: m.id, file: m.file, note: m.note, expect: [...new Set(m.runs.flatMap(x => x.expect))], hold: [...new Set(m.runs.flatMap(x => x.hold))], holdOnly: m.holdOnly };
    if (p.error) {
      console.log(`EDIT-ERROR      ${m.key} [${m.file}] ${p.error}`);
      r.mutations.push({ ...rec, status: 'EDIT-ERROR', why: [p.error], flipped: {} });
    } else {
      if (opt.dryRun) console.log(`EDIT-OK         ${m.key} [${m.file}]${m.also.length ? ' +' + m.also.map(a => a.file).join(',') : ''}`);
      prepared.push({ s, r, m, dir: p.dir, rec });
    }
  }
}
if (opt.dryRun) {
  const errs = results.reduce((n, r) => n + r.mutations.filter(m => m.status === 'EDIT-ERROR').length, 0);
  console.log(`\n${prepared.length} edits applied, ${errs} edit errors; mutated sources in ${OUT}`);
  process.exit(errs ? 1 : 0);
}

// ---- (2) executables
const exes = {};
for (const bin of new Set(prepared.flatMap(({ s, m }) => m.runs.map(x => binOf(s.suite, x.testFn))))) {
  try {
    exes[bin] = opt.exeDir ? findInDir(path.resolve(opt.exeDir), bin) : findViaCargo(ROOT, bin, { release: !!opt.release });
  } catch (e) {
    die(e.message);
  }
  console.log(`exe ${bin}: ${exes[bin]}`);
}

// ---- (3) baseline: each distinct (suite, test fn) once, unmutated
let baselineFailed = false;
const dirtyTests = new Set(); // "suite|testFn"
if (opt.baseline || !opt.noBaseline) {
  const items = new Map(); // key -> { s, r, testFn, expect:Set }
  for (const { s, r, m } of prepared) {
    for (const run of m.runs) {
      const key = `${s.suite.name}|${run.testFn}`;
      if (!items.has(key)) items.set(key, { s, r, testFn: run.testFn, expect: new Set() });
      [...run.expect, ...run.hold].forEach(id => items.get(key).expect.add(id));
    }
  }
  const tasks = [...items.entries()].map(([key, it]) => async () => {
    const dir = path.join(OUT, it.s.suite.name, '_baseline');
    const own = path.join(dir, fileOf(it.testFn));
    copySources(it.s.suite, own);
    const run = await runTest(exes[binOf(it.s.suite, it.testFn)], it.testFn, own, opt.timeout);
    writeLog(own, it.testFn, run);
    run.parsed = parseOutput(run.stdout, run.stderr, it.s.suite.templateMarker);
    const v = judgeBaseline(it.s.suite, run, [...it.expect]);
    it.r.baseline.push({ testFn: it.testFn, ok: v.ok, why: v.why, secs: run.secs });
    if (!v.ok) {
      baselineFailed = true;
      dirtyTests.add(key);
    }
    console.log(`${v.ok ? 'BASELINE-CLEAN ' : 'BASELINE-DIRTY '} ${it.s.suite.name}:${it.testFn} (${run.secs}s)${v.ok ? '' : ': ' + v.why.join('; ')}`);
    if (v.ok && !opt.keep) fs.rmSync(own, { recursive: true, force: true });
  });
  await pool(tasks, opt.jobs);
  if (opt.baseline) {
    console.log(baselineFailed ? '\nbaseline NOT clean' : '\nbaseline clean');
    process.exit(baselineFailed ? 1 : 0);
  }
}

// ---- (4) mutation runs
const tasks = [];
for (const { s, r, m, dir, rec } of prepared) {
  const state = { runs: [], t0: now() };
  const dirty = m.runs.filter(x => dirtyTests.has(`${s.suite.name}|${x.testFn}`));
  const finalize = () => {
    let v;
    if (dirty.length) v = { status: 'BASELINE-DIRTY', why: [`baseline of ${dirty.map(x => x.testFn).join(', ')} is not clean`], flipped: {}, collateral: [], posFails: [], notes: [] };
    else v = judgeMutation(s.suite, m, state.runs);
    for (const run of state.runs) for (const sec of run.parsed.sections) if (!r.templates.includes(sec.name)) r.templates.push(sec.name);
    const out = { ...rec, ...v, secs: Number(state.runs.reduce((a, x) => a + x.secs, 0).toFixed(1)), runs: state.runs.map(x => ({ testFn: x.testFn, exit: x.code, secs: x.secs })) };
    r.mutations.push(out);
    const flips = Object.entries(v.flipped).map(([t, l]) => `${t ? t.replace(/^kron_token_|\.bin$/g, '') + ': ' : ''}${l.join(' ')}`).join(' | ');
    const tag = v.status.padEnd(14);
    console.log(`${tag} ${m.key} [${m.file}] ${[...rec.expect, ...rec.hold.map(h => '!' + h)].join(',')} :: ${flips || '-'}${v.why.length ? ' -- ' + v.why.join('; ') : ''}${v.notes?.length ? ' (' + v.notes.join('; ') + ')' : ''} (${out.secs}s)`);
    if ((v.status === 'CONFIRMED' || v.status === 'HELD') && !opt.keep) fs.rmSync(dir, { recursive: true, force: true });
  };
  if (dirty.length === m.runs.length) {
    tasks.push(async () => finalize());
    continue;
  }
  let pending = m.runs.length;
  for (const run of m.runs) {
    tasks.push(async () => {
      const res = await runTest(exes[binOf(s.suite, run.testFn)], run.testFn, dir, opt.timeout);
      writeLog(dir, run.testFn, res);
      res.parsed = parseOutput(res.stdout, res.stderr, s.suite.templateMarker);
      state.runs.push({ testFn: run.testFn, expect: run.expect, hold: run.hold, ...res });
      if (--pending === 0) {
        state.runs.sort((a, b) => m.runs.findIndex(x => x.testFn === a.testFn) - m.runs.findIndex(x => x.testFn === b.testFn));
        finalize();
      }
    });
  }
}
await pool(tasks, opt.jobs);

// ---- (5) report
for (const r of results) {
  const catalogOrder = byName[r.name].s.mutations.map(m => m.id);
  r.mutations.sort((a, b) => catalogOrder.indexOf(a.id) - catalogOrder.indexOf(b.id));
  r.templates.sort();
}
const meta = { date: new Date().toISOString().slice(0, 10), secs: secs(T0), jobs: opt.jobs };
const t = writeReport(OUT, meta, results);
for (const s of suites) {
  try {
    fs.rmdirSync(path.join(OUT, s.suite.name, '_baseline'));
  } catch {}
  try {
    fs.rmdirSync(path.join(OUT, s.suite.name));
  } catch {}
}
console.log(`\n${totalsLine(t)}; results in ${path.join(OUT, 'results.md')} and results.json (${meta.secs}s)`);
const bad = t.failed + t.editErrors + (baselineFailed ? 1 : 0);
process.exit(bad ? 1 : 0);
