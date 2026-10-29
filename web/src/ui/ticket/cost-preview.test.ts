// The cost preview under the amount box: a limit order is amount x price; a market order walks the book from the best level and shows the bound it allows
// beside (a buy's max plus the fee, a sell's min less it) only when they differ; a book too thin for the amount says what it covers.
import { describe, expect, it } from 'vitest';
import type { Intent } from '../../kob/plan';
import type { BookView } from '../../kob/plan-types';
import { costLine, costPreview, type CostPreview } from './cost-preview';

const SCALE = 1000n; // 3 decimals: 1000 base units per whole token
const KAS = 100_000_000n;
const TOK = 1000n;
const book: BookView = {
  asks: [
    { price: 2n * KAS, amount: 3n * TOK, orders: 1 },
    { price: 3n * KAS, amount: 10n * TOK, orders: 2 },
  ],
  bids: [
    { price: 2n * KAS - KAS / 10n, amount: 4n * TOK, orders: 1 },
    { price: KAS, amount: 10n * TOK, orders: 1 },
  ],
};
const base = { book, scale: SCALE, worstPrice: null, fee: null } as const;
const limit = (side: 'buy' | 'sell', amount: bigint, price: bigint): Intent => ({ type: 'limit', side, amount, price }) as Intent;
const market = (side: 'buy' | 'sell', amount: bigint): Intent => ({ type: 'market', side, amount }) as Intent;

const fmt = (v: bigint) => `${(Number(v) / Number(KAS)).toFixed(2)} KAS`;
const fmtBase = (v: bigint) => `${Number(v) / 1000} TUSD`;
const words: Record<string, string> = { dash: '—', exact: '{value}', buy: '≈ {estimate} · max {bound}', sell: '≈ {estimate} · min {bound}', thin: 'book covers only {amount}' };
const tr = (key: string, params: Record<string, string>) => words[key]!.replace(/\{(\w+)\}/g, (_m, k: string) => params[k] ?? '');
const line = (p: CostPreview) => costLine(p, fmt, fmtBase, tr);

describe('the cost preview includes its own tip and the fee (audit 2026-10-29 round 2, R4)', () => {
  const TIP = KAS / 2n;
  const withTip = (intent: Intent): Intent => ({ ...intent, tip: TIP }) as Intent;
  it('a limit buy pays price + tip per token, a limit sell receives price - tip', () => {
    expect(costPreview({ ...base, intent: withTip(limit('buy', 10n * TOK, 23n * KAS / 10n)) })).toMatchObject({ estimate: 10n * (23n * KAS / 10n + TIP) });
    expect(costPreview({ ...base, intent: withTip(limit('sell', 10n * TOK, 26n * KAS / 10n)) })).toMatchObject({ estimate: 10n * (26n * KAS / 10n - TIP) });
  });
  it('the fee of a limit order is part of the total: a buy pays it on top, a sell receives it less', () => {
    expect(costPreview({ ...base, intent: limit('buy', 10n * TOK, 2n * KAS), fee: KAS / 100n })).toMatchObject({ estimate: 20n * KAS + KAS / 100n, bound: null });
    expect(costPreview({ ...base, intent: limit('sell', 10n * TOK, 2n * KAS), fee: KAS / 100n })).toMatchObject({ estimate: 20n * KAS - KAS / 100n, bound: null });
  });
  it('a market order: the walk and the bound both carry the tip', () => {
    // 5 tokens: 3 at 2 KAS and 2 at 3 KAS = 12 KAS, plus 0.5 KAS a token of tip
    const p = costPreview({ ...base, intent: withTip(market('buy', 5n * TOK)), worstPrice: 3n * KAS + KAS / 10n });
    expect(p).toEqual({ kind: 'cost', side: 'buy', estimate: 12n * KAS + 5n * TIP, bound: 5n * (3n * KAS + KAS / 10n + TIP) });
    // a sell's tip is paid out of its proceeds
    const q = costPreview({ ...base, intent: withTip(market('sell', 6n * TOK)), worstPrice: 9n * KAS / 10n });
    expect(q).toMatchObject({ estimate: 96n * KAS / 10n - 6n * TIP });
  });
  it('is the total the plan discloses (all-in + fee), tip and fee included', async () => {
    const { planOrder } = await import('../../kob/plan');
    const { TOKEN, ticketEnv } = await import('./ticket-fixtures');
    const e = ticketEnv({ funding: [1_000_000n * KAS] });
    for (const side of ['buy', 'sell'] as const) {
      const intent = { type: 'limit', side, amount: 10n * TOKEN, price: side === 'buy' ? 230_000_000n : 260_000_000n, tip: TIP } as Intent;
      const p = planOrder(e, intent);
      expect(p.ok).toBe(true);
      const pv = costPreview({ intent, book: e.book, scale: e.token.scale, worstPrice: null, fee: null }) as { estimate: bigint };
      expect(pv.estimate).toBe(p.disclosure!.allInTotal);
    }
    const intent = { type: 'market', side: 'buy', amount: 10n * TOKEN, tip: TIP } as Intent;
    const p = planOrder(e, intent);
    const pv = costPreview({ intent, book: e.book, scale: e.token.scale, worstPrice: p.disclosure!.worstPrice ?? null, fee: p.disclosure!.fee ?? null }) as { bound: bigint };
    expect(pv.bound).toBe((p.disclosure!.allInTotal ?? 0n) + (p.disclosure!.fee ?? 0n));
  });
});

describe('the cost preview', () => {
  it('is empty without an amount, without an intent, and for orders that are no plain trade', () => {
    expect(costPreview({ ...base, intent: null })).toEqual({ kind: 'empty' });
    expect(costPreview({ ...base, intent: limit('buy', 0n, KAS) })).toEqual({ kind: 'empty' });
    expect(costPreview({ ...base, intent: { type: 'twap', amount: 5n, price: KAS } as Intent })).toEqual({ kind: 'empty' });
    expect(line({ kind: 'empty' })).toBe('—');
  });

  it('a limit order is amount x its price, one value', () => {
    const p = costPreview({ ...base, intent: limit('buy', 5n * TOK, 25n * KAS / 10n) });
    expect(p).toEqual({ kind: 'cost', side: 'buy', estimate: 125n * KAS / 10n, bound: null });
    expect(line(p)).toBe('12.50 KAS');
    expect(line(costPreview({ ...base, intent: limit('sell', 2n * TOK, KAS) }))).toBe('2.00 KAS');
  });

  it('a market buy walks the asks from the best level; the bound is the worst price it allows plus the fee', () => {
    // 5 tokens: 3 at 2 KAS and 2 at 3 KAS = 12 KAS
    const p = costPreview({ ...base, intent: market('buy', 5n * TOK), worstPrice: 3n * KAS + KAS / 10n, fee: KAS / 100n });
    expect(p).toEqual({ kind: 'cost', side: 'buy', estimate: 12n * KAS + KAS / 100n, bound: 5n * (3n * KAS + KAS / 10n) + KAS / 100n });
    expect(line(p)).toBe('≈ 12.01 KAS · max 15.51 KAS');
  });

  it('a market sell walks the bids; the bound is its min less the fee', () => {
    // 6 tokens: 4 at 1.9 KAS and 2 at 1 KAS = 9.6 KAS; the order allows 0.9 KAS at worst
    const p = costPreview({ ...base, intent: market('sell', 6n * TOK), worstPrice: 9n * KAS / 10n, fee: KAS / 100n });
    expect(p).toMatchObject({ kind: 'cost', side: 'sell', estimate: 96n * KAS / 10n - KAS / 100n });
    expect(line(p)).toBe('≈ 9.59 KAS · min 5.39 KAS');
  });

  it('shows one value when the estimate and the bound are the same at the precision shown (no slippage, the best level covers it, no fee)', () => {
    const p = costPreview({ ...base, intent: market('buy', 2n * TOK), worstPrice: 2n * KAS, fee: null });
    expect(line(p)).toBe('4.00 KAS');
    // no bound known yet: the estimate alone, as an estimate
    const q = costPreview({ ...base, intent: market('buy', 2n * TOK) });
    expect(q).toMatchObject({ kind: 'cost', bound: null });
    expect(line(q)).toBe('4.00 KAS');
  });

  it('a book too thin for the amount says what it covers', () => {
    const p = costPreview({ ...base, intent: market('buy', 20n * TOK) });
    expect(p).toEqual({ kind: 'thin', covers: 13n * TOK });
    expect(line(p)).toBe('book covers only 13 TUSD');
    expect(costPreview({ ...base, book: { asks: [], bids: [] }, intent: market('sell', TOK) })).toEqual({ kind: 'thin', covers: 0n });
  });

  it('a priority tip is part of the price: a buy pays it, a sell gives it up', () => {
    const tipped: BookView = { asks: [{ price: 2n * KAS, amount: 3n * TOK, orders: 1, tip: KAS / 10n }], bids: [{ price: KAS, amount: 3n * TOK, orders: 1, tip: KAS / 10n }] };
    expect(costPreview({ ...base, book: tipped, intent: market('buy', 3n * TOK) })).toMatchObject({ estimate: 63n * KAS / 10n });
    expect(costPreview({ ...base, book: tipped, intent: market('sell', 3n * TOK) })).toMatchObject({ estimate: 27n * KAS / 10n });
  });
});
