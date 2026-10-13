#!/usr/bin/env node
// Catalog coverage: which `require` lines of the mutated templates no catalog mutation deletes or changes.
//
//   node tools/ablation/coverage.mjs [--suite a,b|all] [--file REGEX] [--summary] [--root REPO] [--results FILE,FILE [--program NAME]]
//
// A require line (the line of a statement's `require(`) counts as covered when at least one mutation of any selected
// catalog (its `edits` on `file`, or an `also` part on that contract) deletes or changes that very line. Changed lines
// are found with an exact LCS line diff of the committed source (line endings normalised to LF) against the mutated
// text, so a line between two edits of a multi-edit mutation is covered only when it is itself changed. A covered line is
// not necessarily confirmed: run the mutation (ablate.mjs) for that. Templates are grouped by contract name; a contract
// mutated by several suites (KobAsk in kcc20-sell, its evidence role in other suites) collects the mutations of all of
// them. Commented-out lines are ignored.
//
// --results: only the mutations that some of these ablate.mjs `results.json` files report as CONFIRMED or HELD count, so
// a covered line is a confirmed one on the runs those files record (e.g. the runs on the public-mint program, audit
// AUD-PM: the uncovered lines must then be exactly the documented ones of the static check). With --program, a mutation
// counts only when its verdict was seen on that token program (`programs` of the result: the program names printed with
// its flipped, or for a hold its rejected, scenarios), e.g. `--program KCC20PublicMint`.
//
// Output: per template the number of require lines and the uncovered ones (line number and text); --summary prints the
// counts only. Exit code 0 (the list is informational: the catalogs document the unwitnessable checks in their header).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SUITE_NAMES, loadSuite } from './lib/suites.mjs';
import { applyEdits } from './lib/edits.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const opt = k => {
  const i = argv.indexOf(k);
  return i >= 0 ? argv[i + 1] : undefined;
};
// --root: the repository whose contracts the catalogs name (default: the one this script lives in)
const ROOT = path.resolve(opt('--root') ?? path.join(HERE, '..', '..'));
const suiteArg = opt('--suite') ?? 'all';
const suites = suiteArg === 'all' ? SUITE_NAMES : suiteArg.split(',');
const fileRe = opt('--file') ? new RegExp(opt('--file')) : null;
const summary = argv.includes('--summary');
const program = opt('--program') ?? null;
if (program && !opt('--results')) throw new Error('--program needs --results');
// suite:id of every mutation the given results files confirm (CONFIRMED) or hold (HELD); null = every mutation counts
let witnessed = null;
if (opt('--results')) {
  witnessed = new Set();
  for (const f of opt('--results').split(',')) {
    const r = JSON.parse(fs.readFileSync(f, 'utf8'));
    for (const st of r.suites) {
      for (const m of st.mutations) {
        if (m.status !== 'CONFIRMED' && m.status !== 'HELD') continue;
        if (program && !(m.programs ?? []).includes(program)) continue;
        witnessed.add(`${st.name}:${m.id}`);
      }
    }
    console.log(`results ${f}: ${r.date}, KCC-20 3 / 3 program ${r.kcc20Program ?? 'KCC20Ref'}, ${r.suites.map(x => x.name).join(', ')}`);
  }
}

/** 1-based numbers of the lines of `a` that the LCS line diff against `b` does not keep. */
export function changedLines(a, b) {
  const n = a.length;
  const m = b.length;
  let p = 0;
  while (p < n && p < m && a[p] === b[p]) p++;
  let q = 0;
  while (q < n - p && q < m - p && a[n - 1 - q] === b[m - 1 - q]) q++;
  const A = a.slice(p, n - q);
  const B = b.slice(p, m - q);
  const N = A.length;
  const M = B.length;
  const dp = Array.from({ length: N + 1 }, () => new Int32Array(M + 1));
  for (let i = N - 1; i >= 0; i--) {
    for (let j = M - 1; j >= 0; j--) dp[i][j] = A[i] === B[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  }
  const out = new Set();
  let i = 0;
  let j = 0;
  while (i < N && j < M) {
    if (A[i] === B[j]) {
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) out.add(p + ++i);
    else j++;
  }
  while (i < N) out.add(p + ++i);
  return out;
}

const covered = new Map(); // contract -> Set(line)
const sources = new Map(); // contract -> source text
for (const name of suites) {
  const { suite, mutations } = await loadSuite(name, ROOT);
  for (const m of mutations) {
    for (const part of [{ file: m.file, edits: m.edits }, ...m.also]) {
      if (fileRe && !fileRe.test(part.file)) continue;
      const src = fs.readFileSync(path.join(suite.srcAbs, `${part.file}.sil`), 'utf8').replace(/\r\n/g, '\n');
      sources.set(part.file, src);
      if (witnessed && !witnessed.has(m.key)) continue; // its contract is listed, its edits do not count
      let text;
      try {
        text = applyEdits(src, part.edits).text;
      } catch (e) {
        console.log(`EDIT-ERROR ${m.key}: ${e.message}`);
        continue;
      }
      const set = covered.get(part.file) ?? covered.set(part.file, new Set()).get(part.file);
      for (const l of changedLines(src.split('\n'), text.split('\n'))) set.add(l);
    }
  }
}

let total = 0;
let open = 0;
for (const [file, src] of [...sources].sort()) {
  const set = covered.get(file) ?? new Set();
  const miss = [];
  let n = 0;
  src.split('\n').forEach((l, k) => {
    if (!/\brequire\(/.test(l) || /^\s*\/\//.test(l)) return;
    n++;
    if (!set.has(k + 1)) miss.push(`   ${k + 1}: ${l.trim().slice(0, 140)}`);
  });
  total += n;
  open += miss.length;
  console.log(`${file}: ${n} require lines, ${miss.length} not mutated`);
  if (!summary) for (const x of miss) console.log(x);
}
console.log(`total: ${total} require lines, ${open} not ${witnessed ? `witnessed by a confirmed / held mutation of the results${program ? ` on ${program}` : ''}` : 'mutated'} (suites: ${suites.join(', ')})`);
