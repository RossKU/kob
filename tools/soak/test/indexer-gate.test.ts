// The indexer gate: the pure decision (health -> open / closed, transitions) and the gate object with an injected clock and health
// function. The submit choke point itself (BotWallet.submit) only calls `gateRefusal`, which is tested here with a fake gate.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { advance, gateRefusal, IndexerGate, judgeHealth, type GateState, type HealthLike } from '../src/indexer-gate.ts';

const following: HealthLike = { state: 'following', caught_up: true, lag_daa: 3, last_error: null };

// ------------------------------------------------------------------------------------------------ pure decision

test('open only when caught_up is true', () => {
  assert.equal(judgeHealth(following).open, true);
  assert.equal(judgeHealth({ state: 'following', caught_up: false, lag_daa: 5000 }).open, false);
  assert.equal(judgeHealth({ state: 'catching_up', caught_up: false, lag_daa: 1200 }).open, false);
  // caught_up wins over the state text when both are present
  assert.equal(judgeHealth({ state: 'catching_up', caught_up: true }).open, true);
});

test('lag tolerance: following or catching_up within maxLagDaa trades, beyond it, an unknown lag or another state does not', () => {
  const tol = 300;
  const j = (h: HealthLike) => judgeHealth(h, undefined, tol);
  assert.equal(j({ state: 'catching_up', caught_up: false, lag_daa: 300 }).open, true);
  assert.equal(j({ state: 'catching_up', caught_up: false, lag_daa: 301 }).open, false);
  assert.equal(j({ state: 'following', caught_up: false, lag_daa: 250 }).open, true);
  assert.equal(j({ state: 'catching_up', caught_up: false, lag_daa: null }).open, false);
  assert.equal(j({ state: 'catching_up', caught_up: false }).open, false);
  for (const state of ['starting', 'node_unavailable', 'gap', 'stopped']) assert.equal(j({ state, caught_up: false, lag_daa: 5 }).open, false, state);
  // caught_up is still open whatever the lag text says; tolerance 0 is the strict gate
  assert.equal(j({ state: 'following', caught_up: true, lag_daa: 5 }).open, true);
  assert.equal(judgeHealth({ state: 'catching_up', caught_up: false, lag_daa: 10 }, undefined, 0).open, false);
  assert.equal(judgeHealth({ state: 'catching_up', caught_up: false, lag_daa: 10 }).open, false);
  // the reason says it is a tolerated lag, and the closed reason still names state and lag
  assert.match(j({ state: 'catching_up', caught_up: false, lag_daa: 120 }).reason, /catching_up, 120 DAA behind \(within 300\)/);
  assert.equal(j({ state: 'catching_up', caught_up: false, lag_daa: 4000 }).reason, 'catching_up, 4000 DAA behind');
  // an unreachable indexer is closed whatever the tolerance
  assert.equal(judgeHealth(null, new Error('x'), tol).open, false);
});

test('the gate object applies the tolerance: a flicker between caught_up and a fresh catching_up is not a pause; a lag past it is', async () => {
  let h: HealthLike = { state: 'following', caught_up: true, lag_daa: 4 };
  let t = 0;
  const g = new IndexerGate({ health: () => Promise.resolve(h), now: () => t, pollMs: 1000, maxLagDaa: 300 });
  assert.equal((await g.check()).open, true);
  t += 2000;
  h = { state: 'catching_up', caught_up: false, lag_daa: 220 };
  assert.equal((await g.check()).open, true);
  assert.equal(await g.refusal('limit'), null);
  t += 2000;
  h = { state: 'catching_up', caught_up: false, lag_daa: 900 };
  assert.equal((await g.check()).open, false);
  assert.match((await g.refusal('limit'))?.error ?? '', /catching_up, 900 DAA behind/);
  // the strict gate refuses the first of those
  let t2 = 0;
  const strict = new IndexerGate({ health: () => Promise.resolve({ state: 'catching_up', caught_up: false, lag_daa: 220 }), now: () => t2, pollMs: 1000 });
  assert.equal((await strict.check()).open, false);
  t2 += 1;
});

test('an indexer without caught_up is judged by its state', () => {
  assert.equal(judgeHealth({ state: 'following' }).open, true);
  for (const state of ['catching_up', 'node_unavailable', 'gap', 'starting', 'stopped', 'something_new']) assert.equal(judgeHealth({ state }).open, false, state);
  assert.equal(judgeHealth({}).open, false);
});

test('a failed or empty health request is not following', () => {
  const v = judgeHealth(null, new Error('connect ECONNREFUSED 127.0.0.1:8091'));
  assert.equal(v.open, false);
  assert.match(v.reason, /health request failed: connect ECONNREFUSED/);
  assert.equal(judgeHealth(undefined).open, false);
  assert.equal(judgeHealth('nope' as unknown as HealthLike).open, false);
});

test('the reason names state, lag and last error', () => {
  const v = judgeHealth({ state: 'node_unavailable', caught_up: false, lag_daa: 123456, last_error: 'timeout after 180s' });
  assert.deepEqual([v.state, v.lagDaa, v.lastError], ['node_unavailable', 123456, 'timeout after 180s']);
  assert.equal(v.reason, 'node_unavailable, 123456 DAA behind, last error: timeout after 180s');
});

test('transitions: one pause per closed stretch, one resume with the paused duration', () => {
  const open = judgeHealth(following);
  const closed = judgeHealth({ state: 'catching_up', caught_up: false });
  let s: GateState = { open: null, since: 0 };
  const step = (v: ReturnType<typeof judgeHealth>, t: number) => {
    const r = advance(s, v, t);
    s = r.next;
    return r.transition;
  };
  assert.equal(step(open, 1000), null); // a first "open" is silent
  assert.equal(step(open, 2000), null);
  assert.equal(step(closed, 3000)?.kind, 'paused');
  assert.equal(step(closed, 4000), null); // still the same pause
  assert.equal(step(judgeHealth(null, new Error('x')), 5000), null); // another reason, same pause
  const back = step(open, 13_000);
  assert.deepEqual(back && back.kind === 'resumed' ? back.pausedMs : null, 10_000);
  assert.equal(step(open, 14_000), null);
});

test('starting while the indexer lags is a pause right away', () => {
  const r = advance({ open: null, since: 0 }, judgeHealth({ state: 'catching_up', caught_up: false }), 7);
  assert.equal(r.transition?.kind, 'paused');
  assert.deepEqual(r.next, { open: false, since: 7 });
});

// ------------------------------------------------------------------------------------------------ the gate object

interface Rig {
  gate: IndexerGate;
  clock: { t: number };
  answers: (HealthLike | Error)[];
  calls: { n: number };
  logs: { lvl: string; msg: string; f?: Record<string, unknown> }[];
  counters: Record<string, number>;
  gauges: Record<string, unknown>;
}
/** answers are consumed in order; the last one repeats */
function rig(...answers: (HealthLike | Error)[]): Rig {
  const clock = { t: 1_000_000 };
  const calls = { n: 0 };
  const logs: Rig['logs'] = [];
  const counters: Record<string, number> = {};
  const gauges: Record<string, unknown> = {};
  const gate = new IndexerGate({
    now: () => clock.t,
    health: async () => {
      const a = answers[Math.min(calls.n++, answers.length - 1)]!;
      if (a instanceof Error) throw a;
      return a;
    },
    log: { info: (msg, f) => logs.push({ lvl: 'info', msg, f }), warn: (msg, f) => logs.push({ lvl: 'warn', msg, f }) },
    stats: { inc: (k, by = 1) => void (counters[k] = (counters[k] ?? 0) + by), set: (k, v) => void (gauges[k] = v) },
    pollMs: 5000,
  });
  return { gate, clock, answers, calls, logs, counters, gauges };
}

test('pauses on catching_up, node_unavailable and a throwing health request; resumes when following and caught up', async () => {
  const r = rig(
    following,
    { state: 'catching_up', caught_up: false, lag_daa: 9000, last_error: null },
    { state: 'node_unavailable', caught_up: false, lag_daa: null, last_error: 'timeout after 180s' },
    new Error('fetch failed'),
    following,
  );
  const verdicts: boolean[] = [];
  for (let i = 0; i < 5; i++) {
    verdicts.push((await r.gate.check()).open);
    r.clock.t += 5000;
  }
  assert.deepEqual(verdicts, [true, false, false, false, true]);
  assert.equal(r.gate.isOpen(), true);
});

test('each transition is logged once: warn on pause with state, lag and last error; info on resume with the paused time', async () => {
  const r = rig(following, { state: 'catching_up', caught_up: false, lag_daa: 9000, last_error: 'timeout after 180s' }, { state: 'catching_up', caught_up: false, lag_daa: 9100 }, new Error('x'), following, following);
  for (let i = 0; i < 6; i++) {
    await r.gate.check();
    r.clock.t += 5000;
  }
  assert.equal(r.logs.length, 2, JSON.stringify(r.logs));
  assert.equal(r.logs[0]!.lvl, 'warn');
  assert.match(r.logs[0]!.msg, /not following/);
  assert.deepEqual([r.logs[0]!.f!.state, r.logs[0]!.f!.lagDaa, r.logs[0]!.f!.lastError], ['catching_up', 9000, 'timeout after 180s']);
  assert.equal(r.logs[1]!.lvl, 'info');
  assert.match(r.logs[1]!.msg, /resumed/);
  assert.equal(r.logs[1]!.f!.pausedSec, 15); // paused at t+5 s, resumed at t+20 s
  assert.equal(r.counters.gate_paused, 1);
  assert.equal(r.gauges.gate_open, true);
});

test('the verdict is cached for pollMs and concurrent callers share one request', async () => {
  const r = rig(following);
  await Promise.all([r.gate.check(), r.gate.check(), r.gate.check()]);
  assert.equal(r.calls.n, 1);
  r.clock.t += 4999;
  await r.gate.check();
  assert.equal(r.calls.n, 1);
  r.clock.t += 1;
  await r.gate.check();
  assert.equal(r.calls.n, 2);
  await r.gate.check(true);
  assert.equal(r.calls.n, 3);
});

test('refusal: blocked and counted per action while paused, null while following; unknown before the first answer counts as paused', async () => {
  const r = rig({ state: 'catching_up', caught_up: false, lag_daa: 42 }, following);
  assert.equal(r.gate.isOpen(), false);
  const blocked = await r.gate.refusal('mm_buy');
  assert.deepEqual(blocked, { ok: false, error: 'indexer not following: catching_up, 42 DAA behind', paused: true });
  await r.gate.refusal('cancel');
  await r.gate.refusal('mm_buy');
  assert.equal(r.counters.gate_blocked, 3);
  assert.equal(r.counters['gate_blocked:mm_buy'], 2);
  assert.equal(r.counters['gate_blocked:cancel'], 1);
  r.clock.t += 5000;
  assert.equal(await r.gate.refusal('mm_buy'), null);
});

test('the submit choke point: gateRefusal blocks with paused: true and without touching anything else; no gate, no refusal', async () => {
  assert.equal(await gateRefusal(undefined, 'issue'), null);
  const r = rig(new Error('indexer down'), following);
  const out = await gateRefusal(r.gate, 'amend');
  assert.equal(out?.ok, false);
  assert.equal(out?.paused, true);
  assert.match(out!.error, /^indexer not following: health request failed: indexer down/);
  r.clock.t += 5000;
  assert.equal(await gateRefusal(r.gate, 'amend'), null);
});

test('waitOpen sleeps and re-checks until the indexer follows, and gives up when asked to stop', async () => {
  const r = rig({ state: 'catching_up', caught_up: false }, { state: 'catching_up', caught_up: false }, following);
  const sleeps: number[] = [];
  const open = await r.gate.waitOpen(
    () => false,
    async (ms) => {
      sleeps.push(ms);
      r.clock.t += ms;
    },
  );
  assert.equal(open, true);
  assert.deepEqual(sleeps, [5000, 5000]); // two paused polls, then following
  assert.equal(r.calls.n, 3);

  const r2 = rig({ state: 'gap', caught_up: false });
  let stopped = false;
  const done = await r2.gate.waitOpen(
    () => stopped,
    async (ms) => {
      r2.clock.t += ms;
      stopped = true;
    },
  );
  assert.equal(done, false);
});
