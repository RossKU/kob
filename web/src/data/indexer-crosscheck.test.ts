import { describe, expect, it } from 'vitest';
import { compareFacts, crossCheckIndexers, indexerTrust, type IndexerFacts } from './indexer-crosscheck';

const facts = (over: Partial<IndexerFacts> = {}): IndexerFacts => ({ token: { standing: 'unverified', powers: [] }, bestAsk: 250_000_000n, bestBid: 245_000_000n, ...over });
const ID = 'aa'.repeat(32);
const api = (o: { standing?: string; powers?: string[]; ask?: string; bid?: string; listed?: boolean; fail?: boolean }) =>
  ({
    tokens: async () => {
      if (o.fail) throw new Error('down');
      return o.listed === false ? [] : [{ ticker: 'X', covenant_id: ID, standing: o.standing ?? 'unverified', powers: o.powers ?? [], open_asks: 1, open_bids: 1 }];
    },
    book: async () => ({
      token: ID,
      aggregated: true,
      node_daa: 1,
      asks: o.ask ? [{ price: o.ask, amount: '1000', amount_estimated: false, orders: 1, scale: 1000 }] : [],
      bids: o.bid ? [{ price: o.bid, amount: '1000', amount_estimated: false, orders: 1, scale: 1000 }] : [],
    }),
  }) as never;

describe('compareFacts', () => {
  it('identical facts, or prices within the book-lag tolerance, agree', () => {
    expect(compareFacts(facts(), facts(), 'b')).toEqual([]);
    expect(compareFacts(facts(), facts({ bestAsk: 251_000_000n }), 'b')).toEqual([]);
  });
  it('a different standing, powers, best ask / bid or a missing token is a finding', () => {
    expect(compareFacts(facts(), facts({ token: { standing: 'official', powers: [] } }), 'b').map((f) => f.code)).toEqual(['standing-differs']);
    expect(compareFacts(facts(), facts({ token: { standing: 'unverified', powers: ['freeze'] } }), 'b').map((f) => f.code)).toEqual(['powers-differ']);
    expect(compareFacts(facts(), facts({ bestAsk: 2_500_000_000n, bestBid: null }), 'b').map((f) => f.code)).toEqual(['best-ask-differs', 'best-bid-differs']);
    expect(compareFacts(facts(), facts({ token: null }), 'b').map((f) => f.code)).toEqual(['token-missing']);
  });
});

describe('crossCheckIndexers', () => {
  it('a lying primary (official + no powers + 10x price) is caught by an honest second indexer', async () => {
    const liar = api({ standing: 'official', powers: [], ask: '2500000000', bid: '2450000000' });
    const honest = { label: 'https://b.example', api: api({ standing: 'unverified', powers: ['freeze'], ask: '250000000', bid: '245000000' }) };
    const r = await crossCheckIndexers(liar, [honest], ID);
    expect(r.checked).toBe(1);
    expect(r.findings.map((f) => f.code).sort()).toEqual(['best-ask-differs', 'best-bid-differs', 'powers-differ', 'standing-differs']);
    expect(r.findings.every((f) => f.other === 'https://b.example')).toBe(true);
  });
  it('agreeing indexers give no finding; an unreachable verifier is reported, never counted as agreement', async () => {
    const same = () => api({ ask: '250000000', bid: '245000000' });
    expect((await crossCheckIndexers(same(), [{ label: 'b', api: same() }], ID)).findings).toEqual([]);
    const r = await crossCheckIndexers(same(), [{ label: 'b', api: api({ fail: true }) }], ID);
    expect(r).toMatchObject({ checked: 0, findings: [{ code: 'unreachable', other: 'b' }] });
  });
  it('no verifiers: nothing to compare; trust level none / single / multi', async () => {
    expect(await crossCheckIndexers(api({}), [], ID)).toEqual({ checked: 0, findings: [] });
    expect([indexerTrust(false, 0), indexerTrust(true, 0), indexerTrust(true, 2)]).toEqual(['none', 'single', 'multi']);
  });
});
