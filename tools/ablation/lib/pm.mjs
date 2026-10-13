// Audit AUD-PM: the KCC-20 touch batteries also run on the published public-mint build (KCC20PublicMint, 3 / 3), as their
// `<fn>_pm` twins next to the 3 / 3 `<fn>_3x3` ones (crates/kob-tests: kob_v2_tests, kob_v2_buy_tests). Both programs have
// the same token slots, so every id a mutation expects (or holds) on a `_3x3` twin is expected (held) on the `_pm` twin too.
// A mutation that names a battery without its 3 / 3 twin (the hand-written touch entries: side / price rules, the trailing
// step, update's own-token guard) gets both twins, without the ids whose shape a 3 / 3 program refuses (`skip`).

/** run / hold maps of a mutation, `test` + `expect` folded into `run`. */
function runsOf(m) {
  const run = { ...(m.run ?? {}) };
  if (m.test) run[m.test] = [...(run[m.test] ?? []), ...(m.expect ?? [])];
  return { run, hold: m.hold ? { ...m.hold } : undefined };
}

function twin(runs, batteries, skip) {
  const out = { ...runs };
  for (const [t, ids] of Object.entries(runs)) {
    if (batteries.has(t) && !(`${t}_3x3` in runs)) {
      const k = ids.filter(id => !skip.has(id));
      if (k.length) out[`${t}_3x3`] = k;
    }
  }
  for (const [t, ids] of Object.entries(out)) if (t.endsWith('_3x3')) out[`${t.slice(0, -4)}_pm`] = [...ids];
  return out;
}

/**
 * The catalog's mutations with every battery run also on the 3 / 3 twin (where missing) and every `_3x3` run / hold also on
 * the `_pm` twin. `batteries`: the 8/8 battery test fns; `skip`: ids that do not exist on a 3 / 3 program.
 */
export function withPublicMint(mutations, { batteries = new Set(), skip = new Set() } = {}) {
  return mutations.map(m => {
    const { run, hold } = runsOf(m);
    const { test: _t, expect: _e, ...rest } = m;
    const next = { ...rest, run: twin(run, batteries, skip) };
    if (hold) next.hold = twin(hold, batteries, skip);
    if (!Object.keys(next.run).length) delete next.run;
    return next;
  });
}
