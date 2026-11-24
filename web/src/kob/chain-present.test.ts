// The chain's present the web signs times from (R9FB3-1): no single party decides it.
import { describe, expect, it } from 'vitest';
import { DAA_PER_SECOND, PRESENT_MARGIN_DAA, followWitness, indexerPresent, judgePresent } from './chain-present';
import { planOrder } from './plan';
import type { Clock, PlanEnv } from './plan-types';
import type { HealthView } from '../data/indexer-types';
import { planCancel } from './cancel';
import { decodeSigning } from './decode';
import { CLOCK, TOK, makeEnv } from '../testing/fixtures';
import { MAKER, keyUtxo, placeGolden } from '../testing/chain-fixtures';
import { loadKobNode } from './wasm.node';

const NOW_MS = Number(CLOCK.unixSeconds) * 1000;
const TRUE_DAA = CLOCK.daa;
const health = (o: Partial<HealthView> = {}): HealthView => ({
  ok: true, network: 'testnet-10', cursor_daa: Number(TRUE_DAA - 50n), cursor_time_unix_ms: NOW_MS - 5_000, node_daa: null, lag_daa: null, lag_seconds: 5, settle_depth_daa: 100, ...o,
} as HealthView);
const HOUR = 3_600n * DAA_PER_SECOND;
const DAY = 24n * HOUR;

/** The clock a planner gets from a node answering `nodeDaa`, with or without a healthy indexer. */
function clockOf(nodeDaa: bigint, indexer: boolean): Clock {
  const p = judgePresent(nodeDaa, { indexer: indexer ? indexerPresent(health(), NOW_MS) : null, floor: null });
  return { ...CLOCK, daa: nodeDaa, witness: p.verdict, witnessReason: p.reason };
}
const env = (clock: Clock): PlanEnv => makeEnv({ clock });
const intents = {
  'GTD limit': { type: 'limit', side: 'sell', amount: 5n * TOK, price: 900_000_000n, lifetime: { kind: 'gtd', at: CLOCK.unixSeconds + 7n * 86_400n } },
  market: { type: 'market', side: 'sell', amount: 5n * TOK, tip: 100_000n },
  Dutch: { type: 'dutch', side: 'sell', amount: 5n * TOK, price: 300_000_000n, priceEnd: 200_000_000n, duration: { seconds: 600n } },
  'crossing limit (a take)': { type: 'limit', side: 'buy', amount: 2n * TOK, price: 900_000_000n, crossing: 'take' },
} as const;
const codesOf = (p: ReturnType<typeof planOrder>) => p.issues.map((i) => i.code);

describe('the indexer present is its verified cursor advanced by the wall clock, at most 90 s', () => {
  it('reads the header time, caps the advance, and gives none for an unhealthy or stale indexer', () => {
    expect(indexerPresent(health(), NOW_MS)).toBe(TRUE_DAA);
    expect(indexerPresent(health({ cursor_time_unix_ms: NOW_MS - 100_000 }), NOW_MS)).toBe(TRUE_DAA - 50n + 90n * DAA_PER_SECOND);
    expect(indexerPresent(health({ cursor_time_unix_ms: NOW_MS - 200_000 }), NOW_MS)).toBeNull();
    expect(indexerPresent(health({ ok: false }), NOW_MS)).toBeNull();
    expect(indexerPresent(health({ cursor_time_unix_ms: null }), NOW_MS)).toBeNull();
    expect(indexerPresent(health({ chain_proofs_off: true } as Partial<HealthView>), NOW_MS)).toBeNull();
    // the node's DAA the indexer relays is never the witness
    expect(indexerPresent(health({ node_daa: 1 }), NOW_MS)).toBe(TRUE_DAA);
  });
});

describe('a node reporting the DAA wrong builds nothing timed (R9FB3-1)', () => {
  const lies: [string, bigint][] = [['90 days low', -90n * DAY], ['1 hour low', -HOUR], ['1 hour high', HOUR], ['90 days high', 90n * DAY]];
  for (const [what, by] of lies) {
    for (const [name, intent] of Object.entries(intents)) {
      it(`${what}, indexer healthy: ${name} is refused with the disagreement`, () => {
        const p = planOrder(env(clockOf(TRUE_DAA + by, true)), intent as never);
        expect(p.ok).toBe(false);
        expect(codesOf(p)).toContain('CHAIN_TIME_DISAGREE');
      });
    }
    it(`${what}, indexer down: a market, Dutch and crossing order are refused; a resting GTD is marked unverified`, () => {
      for (const n of ['market', 'Dutch', 'crossing limit (a take)'] as const) {
        const p = planOrder(env(clockOf(TRUE_DAA + by, false)), intents[n] as never);
        expect(p.ok, n).toBe(false);
        expect(codesOf(p), n).toContain('CHAIN_TIME_UNVERIFIED_SHORT');
      }
      const gtd = planOrder(env(clockOf(TRUE_DAA + by, false)), intents['GTD limit'] as never);
      expect(codesOf(gtd)).toContain('CHAIN_TIME_UNVERIFIED');
    });
  }

  it('the floor: a node below the DAA of a coin it reported itself disagrees', () => {
    expect(judgePresent(TRUE_DAA - 10n, { indexer: null, floor: TRUE_DAA }).verdict).toBe('disagree');
  });

  it('honest skew just inside the margin passes, just outside is refused', () => {
    for (const s of [PRESENT_MARGIN_DAA, -PRESENT_MARGIN_DAA]) {
      const p = planOrder(env(clockOf(TRUE_DAA + s, true)), intents.market as never);
      expect(codesOf(p)).not.toContain('CHAIN_TIME_DISAGREE');
    }
    const out = planOrder(env(clockOf(TRUE_DAA + PRESENT_MARGIN_DAA + 1n, true)), intents.market as never);
    expect(codesOf(out)).toContain('CHAIN_TIME_DISAGREE');
  });

  it('a later read of the same node carries the verdict and is judged against the first', () => {
    const first = clockOf(TRUE_DAA, true);
    const later = { ...CLOCK, daa: TRUE_DAA + 100n, unixSeconds: CLOCK.unixSeconds + 10n };
    expect(followWitness(later, first).witness).toBe('agreed');
    expect(followWitness({ ...later, daa: TRUE_DAA - HOUR }, first).witness).toBe('disagree');
  });

  it('a cancel needs no present: it is built whatever the node says', () => {
    const kob = loadKobNode();
    const snap = placeGolden(kob, 'create.ask').snapshots[0]!;
    for (const by of [-90n * DAY, 90n * DAY]) {
      const p = planCancel({ kob, maker: MAKER.pk, funding: [keyUtxo(MAKER.pk, 100n * 100_000_000n, 201)], tokenUtxos: [], clock: clockOf(TRUE_DAA + by, true) }, snap);
      expect(p.ok).toBe(true);
    }
  });
});

describe('the decoder checks the built times against the agreed present (R9FB3-1)', () => {
  const kob = loadKobNode();
  const agreed = { daa: TRUE_DAA, unixSeconds: CLOCK.unixSeconds, witness: 'agreed' as const };
  const now = Number(CLOCK.unixSeconds) * 1000;
  // built on a lying clock with no verdict (the planner did not judge it): the decoder must catch it
  const builtAt = (daa: bigint, intent: unknown) => planOrder(makeEnv({ clock: { ...CLOCK, daa } }), intent as never);
  const decodeWith = (p: ReturnType<typeof planOrder>, present: typeof agreed | { daa: bigint; unixSeconds: bigint; witness: 'disagree' }) =>
    decodeSigning({ kob, built: p.built!, maker: MAKER.pk, registry: null, expected: { orders: p.states, present }, now });
  it('a GTC born expired (built 90 days and an hour low) and a Dutch that starts an hour off block; honest ones pass', () => {
    // a GTC counts its 90 days from the DAA: a node 90 days and an hour low makes it born expired
    const gtd = builtAt(TRUE_DAA - 90n * DAY - HOUR, { type: 'limit', side: 'sell', amount: 5n * TOK, price: 900_000_000n, lifetime: { kind: 'gtc' } } as never);
    expect(gtd.issues.filter((i) => i.severity === "error").map((i) => i.code + ": " + i.message)).toEqual([]);
    expect(decodeWith(gtd, agreed).blocking.map((b) => b.code)).toContain('chain-time');
    const dutch = builtAt(TRUE_DAA + HOUR, intents.Dutch);
    expect(decodeWith(dutch, agreed).blocking.map((b) => b.code)).toContain('chain-time');
    for (const n of ['GTD limit', 'Dutch'] as const) {
      const ok = builtAt(TRUE_DAA, intents[n]);
      expect(decodeWith(ok, agreed).blocking.map((b) => b.code), n).not.toContain('chain-time');
    }
  });
  it('a present the node and the indexer disagree on blocks every new order', () => {
    const p = builtAt(TRUE_DAA, intents['GTD limit']);
    expect(decodeWith(p, { daa: TRUE_DAA, unixSeconds: CLOCK.unixSeconds, witness: 'disagree' }).blocking.map((b) => b.code)).toContain('chain-time');
  });
});
