// The start of a market / close order is the best price of the indexer book. The last fill comes from the same indexer, so only a further
// indexer is an independent reference: without one a large order waits for an acknowledgement, a configured indexer that does not answer is
// said, and an acknowledgement covers the gap (or start) it was given for and nothing worse.
import { describe, expect, it } from 'vitest';
import { marketStartAckOf, planOrder } from './plan';
import { errors } from './plan-types';
import type { PlanIssue } from './plan-types';
import { TOK, level, makeEnv } from '../testing/fixtures';
import { makePairEnv } from './orders/pair-fixtures';
import { newWarnings } from '../ui/ticket/plan-expected';
import { readPairReferences, readReferences } from '../data/indexer-crosscheck';
import { resolveConfig } from '../config';

const REAL = 250_000_000n;
const FAKE = (REAL * 19n) / 10n;
const buy = { type: 'market', side: 'buy', amount: 2n * TOK } as never;
const codes = (p: { issues: { code: string }[] }) => p.issues.map((i) => i.code);
const envAt = (ask: bigint, lastFill: bigint | null, extra: Record<string, unknown> = {}) => {
  const env = makeEnv({ book: { asks: [level(ask, 1_000n * TOK, 1)], bids: [level(REAL, 1_000n * TOK, 1)] } });
  env.lastFillPrice = lastFill;
  // every order counts as large here (the default threshold is 100 KAS)
  return Object.assign(env, { marketStartUnverifiedMinSompi: 1n }, extra);
};
const find = (p: { issues: PlanIssue[] }, code: string) => p.issues.find((i) => i.code === code)!;

describe('a market start with no independent reference', () => {
  it('the indexer reports its last fill at its own start: a large order is held until acknowledged', () => {
    const p = planOrder(envAt(FAKE, FAKE), buy);
    expect(p.ok).toBe(false);
    expect(errors(p).map((i) => i.code)).toEqual(['MARKET_START_UNVERIFIED']);
    const acked = planOrder(envAt(FAKE, FAKE, { marketStartAck: marketStartAckOf(find(p, 'MARKET_START_UNVERIFIED')) }), buy);
    expect(acked.ok).toBe(true);
    expect(codes(acked)).toContain('MARKET_START_UNVERIFIED_ACKNOWLEDGED');
  });

  it('the indexer reports no trades: held the same way (not only a warning)', () => {
    const p = planOrder(envAt(FAKE, null), buy);
    expect(p.ok).toBe(false);
    expect(codes(p)).toContain('MARKET_START_UNVERIFIED');
  });

  it('a small order is told that nothing independent checks the start (a warning)', () => {
    const p = planOrder(envAt(FAKE, FAKE, { marketStartUnverifiedMinSompi: 10n ** 12n }), buy);
    expect(p.ok).toBe(true);
    expect(find(p, 'MARKET_START_NO_INDEPENDENT_REFERENCE').severity).toBe('warning');
  });

  it('the acknowledgement covers that start only: a costlier start, or the other side, is held again', () => {
    const first = planOrder(envAt(FAKE, null), buy);
    const ack = marketStartAckOf(find(first, 'MARKET_START_UNVERIFIED'));
    expect(planOrder(envAt(FAKE + 1n, null, { marketStartAck: ack }), buy).ok).toBe(false);
    expect(planOrder(envAt(FAKE - 1n, null, { marketStartAck: ack }), buy).ok).toBe(true);
    const sell = { type: 'market', side: 'sell', amount: 2n * TOK } as never;
    expect(codes(planOrder(envAt(FAKE, null, { marketStartAck: ack }), sell))).toContain('MARKET_START_UNVERIFIED');
  });

  it('a further indexer that does not answer is said, and the start counts as unchecked by it', () => {
    const p = planOrder(envAt(FAKE, FAKE, { referenceTouches: [], referencesUnavailable: ['https://idx2.example'] }), buy);
    expect(find(p, 'MARKET_REFERENCE_INDEXER_UNAVAILABLE').params).toMatchObject({ others: 'https://idx2.example' });
    expect(codes(p)).toContain('MARKET_START_UNVERIFIED');
    // an answering one that agrees: checked
    const ok = planOrder(envAt(FAKE, FAKE, { referenceTouches: [{ label: 'https://idx3.example', bestAsk: FAKE, bestBid: REAL }] }), buy);
    expect(ok.ok).toBe(true);
    expect(codes(ok).filter((c) => c.startsWith('MARKET_START'))).toEqual([]);
  });
});

describe('a pair market', () => {
  it('without a further indexer a large pair market order is held; one from a further indexer checks it', () => {
    const lone = makePairEnv({ independentReference: false });
    const order = { type: 'market', side: 'buy', amount: 50n * lone.token.scale } as never;
    expect(codes(planOrder(lone, order))).toContain('MARKET_START_UNVERIFIED');
    const checked = planOrder(makePairEnv(), order);
    expect(codes(checked).filter((c) => c.startsWith('MARKET_START'))).toEqual([]);
  });
});

describe('an acknowledged gap is bound to its size', () => {
  it('an acknowledgement of a 12 % gap does not cover a 90 % gap read again at Review', () => {
    const shown = planOrder(envAt((REAL * 112n) / 100n, REAL), buy);
    const ack = marketStartAckOf(find(shown, 'MARKET_START_VS_LAST_FILL'));
    expect(ack).toMatchObject({ kind: 'gap', direction: 'above', bps: 1_200n });
    const shownAcked = planOrder(envAt((REAL * 112n) / 100n, REAL, { marketStartAck: ack }), buy);
    expect(shownAcked.ok).toBe(true);
    const fresh = planOrder(envAt(FAKE, REAL, { marketStartAck: ack }), buy);
    expect(fresh.ok).toBe(false);
    expect(errors(fresh).map((i) => i.code)).toEqual(['MARKET_START_VS_LAST_FILL']);
    // a smaller gap stays covered
    expect(planOrder(envAt((REAL * 111n) / 100n, REAL, { marketStartAck: ack }), buy).ok).toBe(true);
  });

  it('Review shows a warning again when it got worse, not only when its code is new', () => {
    const w = (percent: string): PlanIssue => ({ code: 'PRICE_AGGRESSIVE_VS_MARKET', severity: 'warning', message: '', params: { percent } }) as unknown as PlanIssue;
    expect(newWarnings({ issues: [w('12')] }, { issues: [w('90')] }).map((i) => i.params?.percent)).toEqual(['90']);
    expect(newWarnings({ issues: [w('12')] }, { issues: [w('12')] })).toEqual([]);
    expect(newWarnings({ issues: [w('12')] }, { issues: [w('5')] })).toEqual([]);
  });
});

describe('references from further indexers', () => {
  const book = (ask: bigint | null, bid: bigint | null) => ({ asks: ask === null ? [] : [{ price: ask }], bids: bid === null ? [] : [{ price: bid }] });
  const api = (books: Record<string, ReturnType<typeof book>>) => ({ book: async (id: string) => { const b = books[id]; if (!b) throw new Error('down'); return b as never; } });

  it('a KAS market: an indexer that fails is reported unavailable, never dropped silently', async () => {
    const r = await readReferences(
      [{ label: 'up', api: api({ a: book(10n, 9n) }) }, { label: 'down', api: api({}) }],
      'a' as never,
      (v) => v as unknown as ReturnType<typeof book>,
    );
    expect(r.touches).toEqual([{ label: 'up', bestAsk: 10n, bestBid: 9n }]);
    expect(r.unavailable).toEqual(['down']);
  });

  it('a pair A/B: the touches the two KAS books of each indexer imply', async () => {
    // A: ask 300, bid 200 sompi per whole A; B: ask 20, bid 10 sompi per whole B; scale(B) = 100
    const r = await readPairReferences(
      [{ label: 'x', api: api({ a: book(300n, 200n), b: book(20n, 10n) }) }, { label: 'half', api: api({ a: book(300n, 200n) }) }],
      { covenantId: 'a' as never, toBook: (v) => v as unknown as ReturnType<typeof book> },
      { covenantId: 'b' as never, toBook: (v) => v as unknown as ReturnType<typeof book>, scale: 100n },
    );
    // buying A with B: A's ask over B's bid; selling A for B: A's bid over B's ask (B base units per whole A)
    expect(r.touches).toEqual([{ label: 'x', bestAsk: 3_000n, bestBid: 1_000n }]);
    expect(r.unavailable).toEqual(['half']);
  });
});

describe('stored settings beyond the node and the indexer are named in the banner', () => {
  it('stored extraIndexerUrls', () => {
    const r = resolveConfig({ file: { indexerUrl: 'https://idx.example' }, settings: { extraIndexerUrls: ['https://other.example'] } });
    expect(r.config.extraIndexerUrls).toEqual(['https://other.example']);
    expect(r.storedOverrides).toEqual([{ field: 'extraIndexerUrls', value: 'https://other.example', deployment: '' }]);
    // the same list as the deployment's: nothing to say
    const same = resolveConfig({ file: { extraIndexerUrls: ['https://other.example'] }, settings: { extraIndexerUrls: ['https://other.example'] } });
    expect(same.storedOverrides).toEqual([]);
  });

  it('stored allowQueryOverrides', () => {
    const r = resolveConfig({ file: { indexerUrl: 'https://idx.example' }, settings: { allowQueryOverrides: true }, query: '?indexer=https://other.example' });
    expect(r.storedOverrides).toContainEqual({ field: 'allowQueryOverrides', value: 'true', deployment: 'false' });
    expect(resolveConfig({ file: { allowQueryOverrides: true }, settings: { allowQueryOverrides: true } }).storedOverrides).toEqual([]);
  });
});
