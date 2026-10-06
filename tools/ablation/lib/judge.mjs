// Verdicts: decide from parsed run output whether a mutation is confirmed, or a baseline is clean.
import { idState } from './parse.mjs';

const short = n => (n === '' ? '' : n.replace(/^kron_token_/, '').replace(/\.bin$/, ''));
const at = s => (s.name === '' ? '' : `@${short(s.name)}`);

/** Common sanity of a run: the right number of template sections, the test really ran, no panic. Returns problem strings. */
function runProblems(suite, run) {
  const p = run.parsed;
  const out = [];
  if (run.timedOut) out.push('timed out');
  if (p.ran === 0) out.push('0 tests ran (unknown test function name?)');
  if (p.compileError) out.push('build error');
  if (p.panics.length) out.push(`panic: ${p.panics[0]}`);
  else if (run.code !== 0 && !run.timedOut) out.push(`exit ${run.code ?? run.signal}`);
  if (p.sections.length !== suite.expectedTemplates) out.push(`${p.sections.length}/${suite.expectedTemplates} template sections seen`);
  return out;
}

/**
 * @param suite  suite config
 * @param mut    normalized mutation
 * @param runs   [{ testFn, expect, parsed, code, signal, timedOut }] one per test function of the mutation
 */
export function judgeMutation(suite, mut, runs) {
  const why = [];
  const flipped = {}; // section name -> ["ID", "ID(F)"...]
  const posFails = new Set();
  const collateral = new Set();
  const notes = [];
  for (const run of runs) {
    const expect = new Set([...run.expect, ...run.hold]);
    const problems = runProblems(suite, run);
    for (const s of run.parsed.sections) {
      const list = (flipped[s.name] ??= []);
      for (const p of s.passes) {
        const tag = p.allOk ? p.id : `${p.id}(F)`;
        if (!list.includes(tag)) list.push(tag);
        if (!expect.has(p.id)) collateral.add(p.id);
      }
      for (const pf of s.posFails) posFails.add(pf.id);
      for (const id of run.expect) {
        const st = idState(s, id);
        if (st === 'partial' && mut.inputOnly.includes(id)) continue; // proven by the attacked input alone
        if (st !== 'flipped') why.push(`${id}${at(s)}: ${st === 'absent' ? 'not reached' : st === 'rejected' ? 'still rejected' : st}`);
      }
      for (const id of run.hold) {
        const st = idState(s, id);
        if (st !== 'rejected') why.push(`${id}${at(s)}: ${st === 'absent' ? 'not reached' : 'flipped (' + st + ')'}, expected to stay rejected`);
      }
    }
    // Problems (panic, missing section) only matter when they explain a missing verdict or when they hide a scenario.
    const missing = why.length > 0 || run.parsed.sections.length !== suite.expectedTemplates;
    if (problems.length) (missing ? why : notes).push(`${run.testFn}: ${problems.join('; ')}`);
  }
  let status;
  if (why.length) status = 'NOT-CONFIRMED';
  else status = mut.holdOnly ? 'HELD' : 'CONFIRMED';
  return { status, why, notes, flipped, collateral: [...collateral], posFails: [...posFails] };
}

/** Baseline (no mutation): zero ABLATION-PASS / ABLATION-POS-FAIL, clean exit, every expected attack reached and rejected. */
export function judgeBaseline(suite, run, expectIds) {
  const why = [];
  const p = run.parsed;
  for (const problem of runProblems(suite, run)) why.push(problem);
  for (const s of p.sections) {
    for (const x of s.passes) why.push(`${x.id}${at(s)}: attack ACCEPTED by the committed contract`);
    for (const x of s.posFails) why.push(`${x.id}${at(s)}: positive scenario failed`);
    for (const id of expectIds) if (idState(s, id) === 'absent') why.push(`${id}${at(s)}: scenario not reached (no NEGATIVE line)`);
  }
  return { ok: why.length === 0, why, attacks: p.sections.length ? p.sections[0].negatives.length : 0 };
}
