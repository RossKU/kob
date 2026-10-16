// The start of a market / close order is the best price of the indexer book. The last fill comes from the same indexer, so only a further
// indexer is an independent reference: a pair market without one is accepted, one that answers checks it; a configured indexer that does not
// answer is reported, never dropped silently.
import { describe, expect, it } from 'vitest';
import { planOrder } from './plan';
import { makePairEnv } from './orders/pair-fixtures';
import { readPairReferences, readReferences } from '../data/indexer-crosscheck';
import { resolveConfig } from '../config';

const codes = (p: { issues: { code: string }[] }) => p.issues.map((i) => i.code);

describe('a pair market', () => {
  it('without a further indexer a pair market order is accepted as the book says; one from a further indexer checks it', () => {
    const lone = makePairEnv({ independentReference: false });
    const order = { type: 'market', side: 'buy', amount: 50n * lone.token.scale } as never;
    const p = planOrder(lone, order);
    expect(p.ok).toBe(true);
    expect(codes(p).filter((c) => c.startsWith('MARKET_START'))).toEqual([]);
    const checked = planOrder(makePairEnv(), order);
    expect(codes(checked).filter((c) => c.startsWith('MARKET_START'))).toEqual([]);
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
