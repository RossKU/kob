import { describe, expect, it } from 'vitest';
import { dropZeroDepthLevels, dropEmptyRows, isEmptyRow } from './book-view';
import { HttpIndexer } from './indexer';
import type { BookOrderView, BookView, DepthView, LevelView } from './indexer-types';

const T = 'ab'.repeat(32);
const lv = (price: string, amount: number, est = false, orders = 1): LevelView => ({ price, amount: String(amount), amount_estimated: est, orders, scale: 1000 });
const ord = (price: string, remaining: number | null, est = false): BookOrderView => ({
  covenant_id: 'cd'.repeat(32), contract: 'KobBid', maker: null, price, tip: null, min_fill: '1', scale: 1000, amount_left: remaining === null ? null : String(remaining),
  amount_estimated: est, status: 'partial', cur_value: '100', expiry_daa: null, expired: false, genesis_daa: 1, confirmations: 1, settled: true,
});
const book = (asks: BookView['asks'], bids: BookView['bids']): BookView => ({ token: T, aggregated: true, node_daa: 1, asks, bids });

describe('bids that cannot fund one base unit never enter the book (data layer)', () => {
  it('drops aggregated levels of amount 0 (the "~0" rows) on both sides and keeps funded ones', () => {
    const v = book([lv('120', 3), lv('130', 0, true)], [lv('110', 2), lv('105', 0, true, 2), lv('100', 5)]);
    const r = dropEmptyRows(v);
    expect(r.bids.map((b) => b.price)).toEqual(['110', '100']);
    expect(r.asks.map((a) => a.price)).toEqual(['120']);
    expect(r).toMatchObject({ token: T, aggregated: true, node_daa: 1 });
  });

  it('drops per-order rows with 0 base units left; an UNKNOWN amount is not zero', () => {
    const r = dropEmptyRows(book([], [ord('110', 0, true), ord('100', 2), ord('90', null, true)]));
    expect(r.bids.map((b) => b.price)).toEqual(['100', '90']);
    expect(isEmptyRow(ord('1', 0))).toBe(true);
    expect(isEmptyRow(ord('1', null))).toBe(false);
    expect(isEmptyRow(lv('1', 0))).toBe(true);
    expect(isEmptyRow(lv('1', 1))).toBe(false);
  });

  it('returns the same object when nothing is dropped and tolerates a malformed body', () => {
    const v = book([lv('120', 3)], [lv('110', 2)]);
    expect(dropEmptyRows(v)).toBe(v);
    const odd = { items: [] } as unknown as BookView;
    expect(dropEmptyRows(odd)).toBe(odd);
  });

  it('drops depth levels of amount 0', () => {
    const d: DepthView = {
      token: T, price_basis: '1', ts: 1, daa: 1,
      bids: [{ price: '110', amount: '0', orders: 1, cum_amount: '0', cum_quote: '0', estimated: true }, { price: '100', amount: '5', orders: 1, cum_amount: '5', cum_quote: '500', estimated: true }],
      asks: [],
    };
    expect(dropZeroDepthLevels(d).bids.map((b) => b.price)).toEqual(['100']);
  });

  it('HttpIndexer.book() and .depth() apply the filter', async () => {
    const body = book([lv('120', 3)], [lv('110', 0, true), lv('100', 4)]);
    const depth = { token: T, price_basis: '1', ts: 1, daa: 1, bids: [{ price: '110', amount: '0', orders: 1, cum_amount: '0', cum_quote: '0', estimated: true }], asks: [] };
    const f = (async (u: string | URL | Request) => new Response(JSON.stringify(String(u).includes('/v1/depth') ? depth : body), { status: 200, headers: { 'content-type': 'application/json' } })) as typeof fetch;
    const ix = new HttpIndexer({ baseUrl: 'https://kob.example/', fetch: f });
    expect((await ix.book(T)).bids.map((b) => (b as LevelView).price)).toEqual(['100']);
    expect((await ix.depth(T))!.bids).toEqual([]);
  });
});

describe('GET /v1/tokens items (registry standing)', () => {
  const tok = (over: Record<string, unknown>) => ({ ticker: 'X', covenant_id: T, template_hash: null, extension_commitment: null, scale: null, decimals: null, open_asks: 0, open_bids: 0, ...over });
  const ixOf = (tokens: unknown[]) =>
    new HttpIndexer({ baseUrl: 'https://kob.example/', fetch: (async () => new Response(JSON.stringify({ tokens }), { status: 200, headers: { 'content-type': 'application/json' } })) as typeof fetch });

  it('passes standing, powers, template_id and an empty ticker through', async () => {
    const [a] = await ixOf([tok({ ticker: '', standing: 'official', powers: ['freeze', 'burn'], template_id: 'kcc20-ref-8x8', family: 'kron' })]).tokens();
    expect(a).toMatchObject({ ticker: '', standing: 'official', powers: ['freeze', 'burn'], template_id: 'kcc20-ref-8x8', family: 'kron' });
  });

  it('a missing standing stays missing (older executor); an unknown one is unverified, never official', async () => {
    const [old, odd] = await ixOf([tok({}), tok({ standing: 'gold' })]).tokens();
    expect(old.standing).toBeUndefined();
    expect(old.powers).toBeUndefined();
    expect(odd.standing).toBe('unverified');
  });

  it('powers keep only short lower-case words, once each', async () => {
    const [a] = await ixOf([tok({ powers: ['freeze', 'freeze', '<b>x</b>', 7, 'x'.repeat(40), 'seize'] })]).tokens();
    expect(a.powers).toEqual(['freeze', 'seize']);
  });
});
