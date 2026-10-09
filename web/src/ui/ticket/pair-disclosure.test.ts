// The pair ticket's disclosure from REAL pair plans (kob/orders/pair-*.ts against a PairPlanEnv): every pair order type renders its notes without
// a hole, the pair rows carry the planner's exact amounts (escrows of A and B, the guaranteed B, the KAS tip, what each exit holds) and a pair stop
// discloses the founder's trigger rule (two KAS books or a resting pair order). Issue sentences of the pair planner format their prices in B.
import { describe, expect, it } from 'vitest';
import { rawEntry, t } from '../../i18n';
import { issueText } from '../../i18n/issue-text';
import { planOrder, type Intent } from '../../kob/plan';
import { errors, type PairOrderPlan } from '../../kob/plan-types';
import { makePairEnv, tokenA, tokenB } from '../../kob/orders/pair-fixtures';
import { pairIssue } from '../../kob/orders/pair-issues';
import { NOTE_REQUIRES, PAIR_NOTE_TAGS, buildDisclosureModel, type DisclosureCtx, type DisclosureRow } from './disclosure-model';

const A = tokenA();
const B = tokenB();
const env = makePairEnv();
const CTX: DisclosureCtx = { ticker: A.ticker, decimals: A.decimals, scale: A.scale, clock: env.clock, quote: { ticker: B.ticker, decimals: B.decimals } };
const AMOUNT = 10n * A.scale;
const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]!);
const row = (rows: DisclosureRow[], id: string) => {
  const r = rows.find((x) => x.id === id);
  if (!r) throw new Error(`no row ${id} in ${rows.map((x) => x.id).join(',')}`);
  return r;
};
const textOf = (v: DisclosureRow['value']): string => (typeof v === 'string' ? v : t(v.key, v.params));

const CASES: [string, unknown][] = [
  ['limit sell', { type: 'limit', side: 'sell', amount: AMOUNT, price: 1_550n }],
  ['limit buy', { type: 'limit', side: 'buy', amount: AMOUNT, price: 1_400n, tip: 1_000n }],
  ['marketable limit (auction)', { type: 'limit', side: 'buy', amount: AMOUNT, price: 1_520n }],
  ['market buy', { type: 'market', side: 'buy', amount: 2n * A.scale }],
  ['ioc sell', { type: 'ioc', side: 'sell', amount: AMOUNT, price: 1_440n }],
  ['dutch sell', { type: 'dutch', side: 'sell', amount: AMOUNT, price: 1_700n, priceEnd: 1_550n, duration: { seconds: 600n } }],
  ['stop sell', { type: 'stopMarket', side: 'sell', amount: AMOUNT, stop: 1_400n }],
  ['stop buy', { type: 'stopMarket', side: 'buy', amount: AMOUNT, stop: 1_600n }],
  ['oco sell', { type: 'oco', side: 'sell', amount: AMOUNT, stop: 1_400n, takeProfit: 1_700n }],
  ['trailing sell', { type: 'trailingStop', side: 'sell', amount: AMOUNT, stop: 1_400n, trail: { step: 10n, gap: 30n } }],
  ['ifd buy', { type: 'ifd', side: 'buy', amount: AMOUNT, entry: { price: 1_401n }, exit: { takeProfit: 1_601n } }],
  ['ifo sell', { type: 'ifo', side: 'sell', amount: AMOUNT, entry: { price: 1_599n }, exit: { takeProfit: 1_399n, stop: 1_700n } }],
  ['ifd stop entry', { type: 'ifd', side: 'buy', amount: AMOUNT, entry: { price: 1_600n, stop: 1_550n }, exit: { stop: 1_450n } }],
  ['repeat ifd', { type: 'repeatIfd', side: 'buy', amount: AMOUNT, entry: { price: 1_400n }, exit: { takeProfit: 1_500n }, repeat: { count: 5n } }],
];

const planned = (intent: unknown): PairOrderPlan => {
  const p = planOrder(env, intent as Intent) as PairOrderPlan;
  expect(errors(p), JSON.stringify(errors(p), (_k, v) => (typeof v === 'bigint' ? v.toString() : v))).toEqual([]);
  return p;
};

describe('pair ticket disclosure (real pair plans)', () => {
  it('every note of every pair order type renders without a hole and quotes only what NOTE_REQUIRES lists', () => {
    for (const [name, intent] of CASES) {
      const p = planned(intent);
      const m = buildDisclosureModel(p, CTX)!;
      expect(m.pair, name).not.toBeNull();
      const tags = m.notes.map((n) => n.tag);
      // the pair planner's own sentences: prices only from the KAS books, the KAS tip, route / netting / inventory
      for (const tag of ['pairPricesFromKasBooks', 'pairTipKas', 'pairRoute', 'pairNetting', 'pairInventory']) expect(tags, `${name}: ${tag}`).toContain(tag);
      for (const n of m.notes) {
        const text = t(n.key, n.params);
        expect(text, `${name}/${n.tag}: ${text}`).not.toMatch(/\{\w+\}/);
        const allowed = new Set([...(NOTE_REQUIRES[n.tag] ?? []), 'ticker']);
        for (const ph of placeholders(rawEntry(n.key)!)) expect(allowed, `${n.tag} quotes {${ph}}`).toContain(ph);
      }
      for (const r of m.pair!.rows) expect(textOf(r.value), `${name}/${r.id}`).not.toMatch(/\{\w+\}/);
    }
    expect(PAIR_NOTE_TAGS.every((tag) => rawEntry(`ticket.note.${tag}`) !== undefined)).toBe(true);
  });

  it('a sell holds A and receives at least the planner\'s B (rounded up); a buy holds its B escrow and pays at most (rounded down)', () => {
    const sell = planned(CASES[0]![1]);
    const ms = buildDisclosureModel(sell, CTX)!;
    expect(row(ms.pair!.rows, 'pairEscrowA').value).toBe(`10 ${A.ticker}`);
    expect(row(ms.pair!.rows, 'pairReceiveMinB').value).toContain(B.ticker);
    expect(sell.pair!.receiveMinB).toBe((AMOUNT * 1_550n + A.scale - 1n) / A.scale);
    const buy = planned(CASES[1]![1]);
    const mb = buildDisclosureModel(buy, CTX)!;
    const ids = mb.pair!.rows.map((r) => r.id);
    expect(ids).toContain('pairEscrowB');
    expect(ids).toContain('pairPayMaxB');
    expect(ids).toContain('pairTipKas');
    expect(buy.pair!.payMaxB).toBe((AMOUNT * 1_400n) / A.scale);
    // the tip is KAS, never folded into the B price
    expect(row(mb.price, 'tip').value).toContain('KAS');
    expect(row(mb.price, 'limit').value).toBe(`14 ${B.ticker} / ${A.ticker}`);
  });

  it('a pair stop discloses the founder\'s trigger rule: the two KAS books imply a rate beyond the stop, or a resting pair order fills at or beyond it', () => {
    const m = buildDisclosureModel(planned(CASES[6]![1]), CTX)!;
    const r = row(m.pair!.rows, 'pairTrigger');
    const text = textOf(r.value);
    expect(text).toContain('arms on fills implying a rate at or below 14 BBB / AAA');
    expect(text).toContain(`a resting sell of ${A.ticker} and buy of ${B.ticker} filled together (each rested 5 s)`);
    expect(text).toContain(`or a resting ${A.ticker} pair sell at or below the stop`);
    expect(m.notes.map((n) => n.tag)).toContain('pairTrigger');
    const buy = textOf(row(buildDisclosureModel(planned(CASES[7]![1]), CTX)!.pair!.rows, 'pairTrigger').value);
    expect(buy).toContain('at or above 16 BBB / AAA');
    // a trailing pair stop also says how it trails
    expect(buildDisclosureModel(planned(CASES[9]![1]), CTX)!.pair!.rows.map((x) => x.id)).toContain('pairTrail');
  });

  it('an if-done pair entry says what each exit holds: the A a buy-first fill bought, the B a sell-first fill received plus its prefund', () => {
    const buyFirst = buildDisclosureModel(planned(CASES[10]![1]), CTX)!;
    expect(textOf(row(buyFirst.pair!.rows, 'pairExitCustody').value)).toContain(`the ${A.ticker} its entry fill bought`);
    expect(buyFirst.notes.map((n) => n.tag)).toContain('pairExitCustody');
    const sellFirst = buildDisclosureModel(planned(CASES[11]![1]), CTX)!;
    expect(textOf(row(sellFirst.pair!.rows, 'pairExitCustody').value)).toContain(`the ${B.ticker} its entry fill received plus that fill's prefund`);
    expect(row(sellFirst.pair!.rows, 'pairEscrowB').labelKey).toBe('ticket.disc.pairPrefundB');
    // a stop entry has the pair trigger rule too
    expect(buildDisclosureModel(planned(CASES[12]![1]), CTX)!.pair!.rows.map((x) => x.id)).toContain('pairTrigger');
  });
});

describe('pair issue sentences: prices in B per whole A, amounts of the token their ticker names', () => {
  const ctx = { tokenDecimals: A.decimals, tokenTicker: A.ticker, quoteDecimals: B.decimals, quoteTicker: B.ticker };
  it('formats the planner\'s raw B prices and token amounts', () => {
    expect(issueText(pairIssue('PAIR_MARKETABLE_AUCTION', { touch: 1_500n, ticker: B.ticker }), ctx)).toContain('from 15 BBB per token');
    expect(issueText(pairIssue('PAIR_MARKET_REFERENCE_SOURCE', { reference: 1_450n, worst: 1_400n, ticker: B.ticker }), ctx)).toContain('worst fill 14 BBB per token');
    expect(issueText(pairIssue('PAIR_INSUFFICIENT_TOKENS', { ticker: B.ticker, needed: 12_345n, have: 100n, shortfall: 12_245n }), ctx)).toBe(
      'Not enough BBB: needs 123.45, you hold 1 (short 122.45).',
    );
    expect(issueText(pairIssue('PAIR_INSUFFICIENT_TOKENS', { ticker: A.ticker, needed: 2n * A.scale, have: 0n, shortfall: 2n * A.scale }), ctx)).toContain('needs 2,');
    expect(issueText(pairIssue('PAIR_PREFUND_SHORT', { needed: 250n, given: 100n, ticker: B.ticker }), ctx)).toContain('needs 2.5 BBB per token, you gave 1');
  });

  it('a shared price finding on a pair speaks B, on a KAS market KAS', () => {
    const self = { code: 'COND_TP_CROSSES', message: 'tp crosses', params: { touch: 1_450n } };
    expect(issueText(self, ctx)).toContain('14.5 BBB');
    expect(issueText(self, { tokenDecimals: A.decimals, tokenTicker: A.ticker })).toContain('KAS');
  });
});
