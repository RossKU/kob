// Indexer failover: which indexer may serve (judge / selectMember: pure) and the controller that switches between them (fake timers, scripted indexers).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import {
  BACKOFF_MAX_MS, CONFIRM_MS, IndexerFailover, KOB1_PAYLOAD_VERSION, RECHECKS, RECHECK_BASE_MS, RECOVER_PROBES, failoverIndexer, judge, selectMember,
  type FailoverMember, type Probe, type Verdict,
} from './indexer-failover';
import type { IndexerApi } from './indexer';
import type { HealthView } from './indexer-types';

const NET = 'testnet-10';
const health = (o: Partial<HealthView> = {}): HealthView => ({
  state: 'following', ok: true, network: NET, cursor_daa: 1000, node_daa: 1000, lag_daa: 0, lag_seconds: 0, settle_depth_daa: 100,
  book_complete: true, payload_version: KOB1_PAYLOAD_VERSION, ...o,
});
const ok = (o: Partial<HealthView> = {}): Probe => ({ health: health(o), error: null });
const down: Probe = { health: null, error: new Error('network') };

describe('the payload version this build writes', () => {
  it('equals the one of crates/kob-protocol', () => {
    const rust = readFileSync(new URL('../../../crates/kob-protocol/src/payload.rs', import.meta.url), 'utf8');
    expect(Number(/pub const PAYLOAD_VERSION: u8 = (\d+);/.exec(rust)?.[1])).toBe(KOB1_PAYLOAD_VERSION);
  });
});

describe('judge: may this indexer serve?', () => {
  const primary = { network: NET, primary: true };
  const backup = { network: NET, primary: false };

  it('a caught-up indexer on our network is usable', () => {
    expect(judge(ok(), primary)).toMatchObject({ usable: true, history: 'complete' });
    expect(judge(ok(), backup)).toMatchObject({ usable: true, history: 'complete' });
  });
  it('unreachable, never probed, wrong network, stale, node unavailable and gap are not', () => {
    expect(judge(down, primary)).toMatchObject({ usable: false, code: 'unreachable' });
    expect(judge(null, backup)).toMatchObject({ usable: false, code: 'loading' });
    expect(judge(ok({ network: 'mainnet' }), backup)).toMatchObject({ usable: false, code: 'wrong-network' });
    expect(judge(ok({ network: 'mainnet' }), primary)).toMatchObject({ usable: false, code: 'wrong-network' });
    expect(judge(ok({ lag_seconds: 900 }), backup)).toMatchObject({ usable: false, code: 'stale' });
    expect(judge(ok({ state: 'node_unavailable', ok: false }), backup)).toMatchObject({ usable: false, code: 'node-unavailable' });
    expect(judge(ok({ state: 'gap', ok: false }), backup)).toMatchObject({ usable: false, code: 'gap' });
  });
  it('not ready: lagging, still starting, catching up beyond the tolerance', () => {
    expect(judge(ok({ lag_seconds: 300 }), backup)).toMatchObject({ usable: false, code: 'lagging' });
    expect(judge(ok({ state: 'starting', lag_seconds: 500, lag_daa: 5000 }), backup)).toMatchObject({ usable: false, code: 'starting' });
    expect(judge(ok({ state: 'catching_up', lag_seconds: 500, lag_daa: 5000 }), backup)).toMatchObject({ usable: false, code: 'catching-up' });
    // a few seconds behind is fine, as for the status bar
    expect(judge(ok({ state: 'catching_up', lag_seconds: 20, lag_daa: 200 }), backup).usable).toBe(true);
  });
  it('a backup must report the payload version of this build; the primary is judged as the status bar judges it', () => {
    expect(judge(ok({ payload_version: KOB1_PAYLOAD_VERSION + 1 }), backup)).toMatchObject({ usable: false, code: 'payload-version' });
    const { payload_version: _drop, ...older } = health();
    expect(judge({ health: older as HealthView, error: null }, backup)).toMatchObject({ usable: false, code: 'payload-version' });
    expect(judge({ health: older as HealthView, error: null }, primary).usable).toBe(true);
  });
  it('book_complete says whether the history is whole', () => {
    expect(judge(ok({ book_complete: false, history_from_daa: 990 }), backup)).toMatchObject({ usable: true, history: 'partial' });
    const { book_complete: _drop, ...older } = health();
    expect(judge({ health: older as HealthView, error: null }, backup).history).toBe('unknown');
  });
});

describe('selectMember', () => {
  const v = (usable: boolean, history: Verdict['history'] = 'complete'): Verdict => ({ usable, code: usable ? 'ok' : 'unreachable', history });

  it('the primary whenever it is usable (recovery included)', () => {
    expect(selectMember([v(true), v(true), v(true)], 0)).toBe(0);
    expect(selectMember([v(true), v(true)], 1)).toBe(0);
  });
  it('primary down: the first usable backup', () => {
    expect(selectMember([v(false), v(false), v(true), v(true)], 0)).toBe(2);
  });
  it('a complete backup before a partial one, whatever the order', () => {
    expect(selectMember([v(false), v(true, 'partial'), v(true, 'complete')], 0)).toBe(2);
    expect(selectMember([v(false), v(true, 'partial'), v(true, 'unknown')], 0)).toBe(2);
    expect(selectMember([v(false), v(true, 'partial'), v(false)], 0)).toBe(1);
  });
  it('the backup in use is kept among equals (no taking turns)', () => {
    expect(selectMember([v(false), v(true), v(true)], 2)).toBe(2);
    expect(selectMember([v(false), v(true), v(true)], 1)).toBe(1);
    // ... but not over a better one
    expect(selectMember([v(false), v(true, 'partial'), v(true)], 1)).toBe(2);
  });
  it('nothing usable: the primary (its own banners apply)', () => {
    expect(selectMember([v(false), v(false), v(false)], 0)).toBe(0);
    expect(selectMember([v(false), v(false), v(false)], 2)).toBe(0);
  });
});

// ------------------------------------------------------------------------------------------------ the controller

/** A scripted indexer: `answer` is what `/v1/health` gives now (a Probe: health or an error). */
class Fake {
  answer: Probe = ok();
  reads = 0;
  health = async (): Promise<HealthView> => {
    this.reads++;
    if (this.answer.error) throw this.answer.error;
    return this.answer.health!;
  };
}

function rig(n = 2) {
  const fakes = Array.from({ length: n }, () => new Fake());
  const members: FailoverMember[] = fakes.map((f, i) => ({ label: i === 0 ? 'https://main.example' : `https://backup${i}.example:8443`, api: { health: f.health } }));
  const fo = new IndexerFailover({ members, network: NET, visible: () => true });
  const seen: number[] = [];
  fo.subscribe((s) => seen.push(s.active));
  return { fakes, fo, seen };
}
/** The status poll's report of the active indexer. */
const report = (fo: IndexerFailover, p: Probe) => fo.report(fo.epoch, p);
const settle = async (ms = 0) => {
  await vi.advanceTimersByTimeAsync(ms);
};
/** Advances the clock a second at a time until `cond` holds (at most 5 minutes). */
const until = async (cond: () => boolean) => {
  for (let i = 0; i < 300 && !cond(); i++) await settle(1_000);
  expect(cond()).toBe(true);
};

describe('IndexerFailover', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('needs a primary and a backup', () => {
    expect(() => new IndexerFailover({ members: [{ label: 'a', api: { health: async () => health() } }], network: NET })).toThrow();
  });

  it('starts on the primary and stays while it is healthy: no probe of the others', async () => {
    const { fakes, fo, seen } = rig(3);
    report(fo, ok());
    report(fo, ok({ lag_seconds: 3 }));
    await settle(60_000);
    expect(fo.state).toMatchObject({ active: 0, onBackup: false, epoch: 0, verifiers: [1, 2] });
    expect(fakes.map((f) => f.reads)).toEqual([0, 0, 0]);
    expect(seen).toEqual([]);
  });

  it('primary down from the start: no waiting, the first usable backup is used (a dead one is skipped)', async () => {
    const { fakes, fo } = rig(3);
    fakes[0]!.answer = down;
    fakes[1]!.answer = down;
    report(fo, down);
    await settle(0);
    expect(fo.state).toMatchObject({ active: 2, onBackup: true, host: 'backup2.example:8443', label: 'https://backup2.example:8443', epoch: 1, historyPartial: false });
    // the primary is not a reference while it does not answer; backup 1 is not one while it is down
    expect(fo.state.verifiers).toEqual([]);
  });

  it('a primary that was healthy is re-checked a bounded number of times, with growing delays, before it is given up', async () => {
    const { fakes, fo } = rig();
    report(fo, ok());
    fakes[0]!.answer = down;
    report(fo, down);
    expect(fo.state.active).toBe(0);
    await settle(RECHECK_BASE_MS - 1);
    expect(fakes[0]!.reads).toBe(0);
    await settle(1); // first re-check, fails
    expect(fakes[0]!.reads).toBe(1);
    expect(fo.state.active).toBe(0);
    await settle(RECHECK_BASE_MS * 2); // second re-check, fails: now the others are probed
    expect(fakes[0]!.reads).toBeGreaterThanOrEqual(2);
    expect(RECHECKS).toBe(2);
    expect(fo.state).toMatchObject({ active: 1, onBackup: true });
  });

  it('a primary that blips and answers well again is not left', async () => {
    const { fakes, fo } = rig();
    report(fo, ok());
    report(fo, down);
    await settle(RECHECK_BASE_MS);
    expect(fakes[0]!.reads).toBe(1); // re-check answers well
    expect(fo.state.active).toBe(0);
    await settle(120_000);
    expect(fo.state).toMatchObject({ active: 0, epoch: 0 });
    expect(fakes[1]!.reads).toBe(0);
  });

  it('primary not ready (lagging, node unavailable) counts like down', async () => {
    for (const bad of [ok({ lag_seconds: 400 }), ok({ state: 'node_unavailable', ok: false }), ok({ state: 'gap', ok: false })]) {
      const { fakes, fo } = rig();
      fakes[0]!.answer = bad;
      report(fo, { health: bad.health, error: null }); // never healthy: no second chance first
      await settle(0);
      expect(fo.state.active, JSON.stringify(bad.health?.state)).toBe(1);
    }
  });

  it('never fails over to an indexer on another network or of another payload version; all down: stays on the primary', async () => {
    const { fakes, fo } = rig(3);
    fakes[0]!.answer = down;
    fakes[1]!.answer = ok({ network: 'mainnet' });
    fakes[2]!.answer = ok({ payload_version: KOB1_PAYLOAD_VERSION + 1 });
    report(fo, down);
    await settle(0);
    expect(fo.state).toMatchObject({ active: 0, onBackup: false, epoch: 0 });
    // a wrong-network indexer is not a reference either
    expect(fo.state.verifiers).toEqual([]);
  });

  it('all down: re-evaluation is rate limited and backs off to a cap, and a backup that comes up is then used', async () => {
    const { fakes, fo } = rig();
    fakes[0]!.answer = down;
    fakes[1]!.answer = down;
    report(fo, down);
    await settle(0);
    const reads = () => fakes[1]!.reads;
    expect(reads()).toBe(1);
    // the status poll keeps reporting every 15 s: that does not make more probes than the backoff allows
    for (let t = 0; t < 600; t += 15) {
      await settle(15_000);
      report(fo, down);
    }
    // 10 minutes: probes at 0, 10, 30, 70, then every <= cap
    expect(reads()).toBeLessThanOrEqual(2 + Math.ceil(600_000 / BACKOFF_MAX_MS));
    expect(reads()).toBeGreaterThan(3);
    expect(fo.state.active).toBe(0);
    fakes[1]!.answer = ok();
    await settle(BACKOFF_MAX_MS + 1_000);
    expect(fo.state).toMatchObject({ active: 1, onBackup: true });
  });

  it('a partial-history backup is used when it is the only one, and flagged; a complete one is preferred', async () => {
    const a = rig(3);
    a.fakes[0]!.answer = down;
    a.fakes[1]!.answer = ok({ book_complete: false, history_from_daa: 990 });
    a.fakes[2]!.answer = ok();
    report(a.fo, down);
    await settle(0);
    expect(a.fo.state).toMatchObject({ active: 2, historyPartial: false });

    const b = rig(3);
    b.fakes[0]!.answer = down;
    b.fakes[1]!.answer = ok({ book_complete: false, history_from_daa: 990 });
    b.fakes[2]!.answer = down;
    report(b.fo, down);
    await settle(0);
    expect(b.fo.state).toMatchObject({ active: 1, historyPartial: true });
  });

  it('the backup in use turns bad: another backup, else the primary when it is back', async () => {
    const { fakes, fo } = rig(3);
    fakes[0]!.answer = down;
    report(fo, down);
    await settle(0);
    expect(fo.state.active).toBe(1);
    fakes[1]!.answer = down;
    report(fo, ok()); // the backup was good at first
    report(fo, down);
    await settle(RECHECK_BASE_MS * 8);
    expect(fo.state.active).toBe(2);
    fakes[2]!.answer = down;
    fakes[0]!.answer = ok();
    report(fo, ok());
    report(fo, down);
    await settle(RECHECK_BASE_MS * 8);
    expect(fo.state.active).toBe(0);
  });

  it('recovery: the primary is used again after RECOVER_PROBES good probes in a row, not after one', async () => {
    const { fakes, fo } = rig();
    fakes[0]!.answer = down;
    report(fo, down);
    await settle(0);
    expect(fo.state.active).toBe(1);
    const epoch = fo.epoch;
    // still down: probed on a growing delay, bounded
    await settle(10 * 60_000);
    expect(fo.state.active).toBe(1);
    expect(fakes[0]!.reads).toBeLessThanOrEqual(2 + Math.ceil((10 * 60_000) / 15_000));
    // up again, but flapping: a good probe, then a bad one, resets the count
    fakes[0]!.answer = ok();
    await until(() => fo.state.verifiers.includes(0)); // the first good probe
    expect(fo.state.active).toBe(1);
    fakes[0]!.answer = down;
    await settle(CONFIRM_MS + 1_000);
    expect(fo.state.active).toBe(1);
    expect(fo.state.verifiers.includes(0)).toBe(false);
    fakes[0]!.answer = ok();
    await settle(BACKOFF_MAX_MS * 2 + CONFIRM_MS * (RECOVER_PROBES + 1));
    expect(fo.state).toMatchObject({ active: 0, onBackup: false });
    expect(fo.epoch).toBeGreaterThan(epoch);
    // and no more probes of the others once home
    const r = fakes[1]!.reads;
    await settle(5 * 60_000);
    expect(fakes[1]!.reads).toBe(r);
  });

  it('the primary on a backup is a reference only while it answers well; it is one again at once when it does', async () => {
    const { fakes, fo } = rig(3);
    fakes[0]!.answer = down;
    report(fo, down);
    await settle(0);
    expect(fo.state).toMatchObject({ active: 1 });
    expect(fo.state.verifiers).toEqual([2]);
    fakes[0]!.answer = ok();
    await until(() => fo.state.verifiers.includes(0));
    // the first good probe: it is a reference again (two are needed to go home)
    expect(fo.state.verifiers).toEqual([0, 2]);
    expect(fo.state.active).toBe(1);
  });

  it('a report made under an older epoch (the answer of the indexer used before a switch) is ignored', async () => {
    const { fakes, fo } = rig();
    fakes[0]!.answer = down;
    const old = fo.epoch;
    report(fo, down);
    await settle(0);
    expect(fo.state.active).toBe(1);
    fo.report(old, down); // late answers of the primary
    fo.report(old, down);
    fo.report(old, down);
    await settle(0);
    expect(fo.state.active).toBe(1);
    expect(fo.epoch).toBe(1);
  });

  it('a hidden page does not probe for recovery until it is shown', async () => {
    const fakes = [new Fake(), new Fake()];
    let visible = true;
    const fo = new IndexerFailover({ members: fakes.map((f, i) => ({ label: `https://h${i}.example`, api: { health: f.health } })), network: NET, visible: () => visible });
    fakes[0]!.answer = down;
    report(fo, down);
    await settle(0);
    expect(fo.state.active).toBe(1);
    visible = false;
    fakes[0]!.answer = ok();
    const before = fakes[0]!.reads;
    await settle(5 * 60_000);
    expect(fakes[0]!.reads).toBe(before);
    visible = true;
    await settle(5 * 60_000);
    expect(fo.state.active).toBe(0);
  });

  it('a probe that never answers is cut off', async () => {
    const hang = { health: (o?: { signal?: AbortSignal }) => new Promise<HealthView>((_r, rej) => o?.signal?.addEventListener('abort', () => rej(new Error('aborted')))) };
    const good = new Fake();
    const fo = new IndexerFailover({ members: [{ label: 'https://a.example', api: hang }, { label: 'https://b.example', api: { health: good.health } }], network: NET, visible: () => true });
    report(fo, down);
    await settle(30_000);
    expect(fo.state.active).toBe(1);
  });
});

describe('failoverIndexer', () => {
  it('is one stable object that answers from the active member, call by call', async () => {
    const mk = (tag: string) => ({ tag, health: async () => health({ node_version: tag }), tokens: async () => [], declaration: undefined }) as unknown as IndexerApi & { tag: string };
    const apis = [mk('main'), mk('backup')];
    const state = { active: 0 };
    const proxy = failoverIndexer({ state } as never, apis);
    expect((await proxy.health()).node_version).toBe('main');
    state.active = 1;
    expect((await proxy.health()).node_version).toBe('backup');
    // a method taken off the proxy keeps its indexer
    const h = proxy.health;
    state.active = 0;
    expect((await h()).node_version).toBe('backup');
    expect((await proxy.health()).node_version).toBe('main');
  });
});
