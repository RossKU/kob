// Audit AUD-PM: the KCC-20 touch batteries also run on the published public-mint build (KCC20PublicMint, 3 / 3), as their
// `<fn>_pm` twins next to the 3 / 3 `<fn>_3x3` ones (crates/kob-tests: kob_v2_tests, kob_v2_buy_tests). Both programs have
// the same token slots, so every id a mutation expects (or holds) on a `_3x3` twin is expected (held) on the `_pm` twin too.

function twin(runs) {
  const out = { ...runs };
  for (const [t, ids] of Object.entries(runs)) if (t.endsWith('_3x3')) out[`${t.slice(0, -4)}_pm`] = [...ids];
  return out;
}

/** The catalog's mutations with every `_3x3` run / hold also on the `_pm` twin. */
export function withPublicMint(mutations) {
  return mutations.map(m => ({ ...m, ...(m.run && { run: twin(m.run) }), ...(m.hold && { hold: twin(m.hold) }) }));
}
