// The signer frame's own summary must show what the user agrees to beyond the net effect: two requests that differ only in an order's price (or
// stop) move the same KAS and tokens, so a summary of the net effect alone shows them identically.
import { describe, expect, it } from 'vitest';
import { summarize } from './summary';
import { goldenRequest } from '../testing/golden';
import { goldenRequest as chainGoldenRequest } from '../testing/chain-fixtures';
import { MAKER_PK, TOKEN_COV_ID } from '../testing/token-fixtures';
import { kob } from '../testing/wallet-fixtures';
import type { ActionRequest, BuiltTx } from '../kob/types';
import { planOrder } from '../kob/plan';
import { MAKER_PK as FIXTURE_MAKER, TOKEN_COV } from '../testing/fixtures';
import { buildIntent } from '../ui/ticket/form-state';
import { CTX, form, ticketEnv } from '../ui/ticket/ticket-fixtures';

const HINTS = [{ covenantId: TOKEN_COV_ID, ticker: 'EXKCC', decimals: 3 }];

function build(name: string, edit: (state: Record<string, string>) => void = () => undefined): BuiltTx {
  const req = goldenRequest(name, MAKER_PK) as ActionRequest & { order: { state: Record<string, string> } };
  edit(req.order.state);
  return kob.build(req);
}

describe('signer summary: order terms', () => {
  it('a sell order at another price shows another summary (the net effect alone is the same)', () => {
    const fair = summarize(kob, build('create.ask'), MAKER_PK, HINTS);
    const cheap = summarize(kob, build('create.ask', (s) => (s.price = (BigInt(s.price) / 1000n).toString())), MAKER_PK, HINTS);
    // the same token movement and KAS locked for this key (the KAS line differs only by the fee of a shorter encoding, a few sompi)
    const same = (x: typeof fair) => x.lines.filter((l) => l.label !== 'KAS');
    expect(same(cheap)).toEqual(same(fair));
    // but the order's terms differ, and the price is on screen
    expect(fair.orders).toHaveLength(1);
    expect(cheap.orders).toHaveLength(1);
    expect(cheap.orders).not.toEqual(fair.orders);
    const price = (o: (typeof fair.orders)[number]) => o.rows.find((r) => /price/i.test(r.label))?.value;
    expect(price(fair.orders[0]!)).toBeDefined();
    expect(price(cheap.orders[0]!)).not.toBe(price(fair.orders[0]!));
  });

  it('names the order, its side and its token by the covenant id (8 + 8 hex), and nests an if-done exit', () => {
    const s = summarize(kob, build('create.ask'), MAKER_PK, HINTS);
    expect(s.orders[0]!.title.toLowerCase()).toContain('sell');
    expect(s.orders[0]!.token).toContain(`${TOKEN_COV_ID.slice(0, 8)}…${TOKEN_COV_ID.slice(-8)}`);
    expect(s.lines.find((l) => l.label.startsWith('EXKCC'))?.label).toContain(`${TOKEN_COV_ID.slice(0, 8)}…`);
    const ifd = summarize(kob, build('create.ifdBid'), MAKER_PK, HINTS);
    expect(ifd.orders).toHaveLength(1);
    expect(ifd.orders[0]!.exit).not.toBeNull();
    expect(ifd.orders[0]!.exit!.rows.length).toBeGreaterThan(0);
  });

  it('a stop at another level shows another summary', () => {
    const a = summarize(kob, build('create.condAsk'), MAKER_PK, HINTS);
    const b = summarize(kob, build('create.condAsk', (s) => (s.stopPrice = (BigInt(s.stopPrice) * 9n / 10n).toString())), MAKER_PK, HINTS);
    expect(b.lines.filter((l) => l.label !== 'KAS')).toEqual(a.lines.filter((l) => l.label !== 'KAS'));
    expect(b.orders).not.toEqual(a.orders);
  });

  it('every order kind of the golden vectors (KAS and pair, new and amended) is listed with its terms', () => {
    const names = ['create.ask', 'create.ask.twap', 'create.ask.dutch', 'create.bid', 'create.bid.dca', 'create.condBid', 'create.ifdAsk.repeat', 'amend.ask', 'amend.bid', 'cancelReplace.ask',
      'pair.create.ask', 'pair.create.bid', 'pair.create.condAsk', 'pair.create.ifdBid', 'pair.create.ifdAsk'];
    for (const name of names) {
      const s = summarize(kob, kob.build(chainGoldenRequest(name, MAKER_PK)), MAKER_PK, HINTS);
      expect(s.orders.length, name).toBeGreaterThan(0);
      for (const o of s.orders) expect(o.rows.length, name).toBeGreaterThan(0);
    }
  });

  it('a cancel creates no order and lists none', () => {
    const s = summarize(kob, kob.build(goldenRequest('cancel.ask', MAKER_PK)), MAKER_PK, HINTS);
    expect(s.orders).toEqual([]);
  });
});

describe('signer summary: timing without a clock', () => {
  it("a plain GTC order's expiry is its DAA score, not an if-done exit's (TN10 2026-10-14 B-6); an exit's sentinel still reads as one", () => {
    const expiry = (name: string, edit: (s: Record<string, string>) => void) =>
      summarize(kob, build(name, edit), MAKER_PK, HINTS).orders[0]!.rows.find((r) => /expir/i.test(r.label))?.value;
    // a GTC order placed on testnet-10 in October 2026: DAA ~595.8M now, + 90 days
    expect(expiry('create.ask', (s) => (s.expiryDaa = '673560000'))).toBe('DAA score 673560000');
    expect(expiry('create.bid', (s) => (s.expiryDaa = '673560000'))).toBe('DAA score 673560000');
    expect(expiry('create.ask', (s) => (s.expiryDaa = (1n << 62n).toString()))).toContain('when the exit is created');
  });
});

describe('signer summary: a token without decimals (no hint)', () => {
  it('says its amounts and prices in whole tokens on the assumed 8 decimals, marked (never "base units"; TN10 2026-10-15 B-13, founder 2026-10-17)', () => {
    const s = summarize(kob, build('create.ask'), MAKER_PK, []);
    const rows = s.orders[0]!.rows;
    expect(rows.find((r) => /amount/i.test(r.label))?.value).toMatch(/^0\.0001 [0-9a-f]{8}…[0-9a-f]{8} \(decimals assumed 8\)$/);
    const price = rows.find((r) => /limit price/i.test(r.label))?.value ?? '';
    expect(price).toMatch(/KAS per [0-9a-f]{8}…[0-9a-f]{8} \(decimals assumed 8\)$/);
    expect(rows.map((r) => r.value).join(' ')).not.toMatch(/base unit/);
    // with the decimals known the price stays per whole token
    const known = summarize(kob, build('create.ask'), MAKER_PK, HINTS).orders[0]!.rows.find((r) => /limit price/i.test(r.label))?.value ?? '';
    expect(known).not.toMatch(/base units/);
  });
});

describe('signer summary: prices that cannot be misread, the flipped view in its own words', () => {
  const plan = (f: ReturnType<typeof form>) => planOrder(ticketEnv(), buildIntent(f, { ...CTX, ref: { buy: 250_000_000n, sell: 245_000_000n } }).intent!).built!;
  const hint = (inverted: boolean) => [{ covenantId: TOKEN_COV, ticker: 'TUSD', decimals: 8, ...(inverted ? { inverted: true } : {}) }];
  const rows = (o: { rows: { label: string; value: string }[] }) => Object.fromEntries(o.rows.map((r) => [r.label, r.value]));

  it('a native limit buy: every price is "X KAS per TOKEN", never the pair notation', () => {
    const s = summarize(kob, plan(form('limit', 'buy', { amount: '5', price: '2.3' })), FIXTURE_MAKER, hint(false));
    const o = s.orders[0]!;
    expect(o.title).toBe('Buy limit order');
    expect(rows(o)['Limit price']).toBe('2.3 KAS per TUSD');
    expect(o.rows.some((r) => / \/ /.test(r.value))).toBe(false);
  });

  it('a market sell of KAS on the flipped view (a chain buy of tokens): said as placed, the prices in TUSD per KAS, the chain terms below', () => {
    const s = summarize(kob, plan({ ...form('market', 'buy', { amount: '20' }), kas: true }), FIXTURE_MAKER, hint(true));
    const o = s.orders[0]!;
    expect(o.title).toBe('Sell KAS · market (auction)');
    expect(o.token).toMatch(/^TUSD \(.+\) · on chain: Buy market order \(auction\)$/);
    const r = rows(o);
    expect(r['You sell']).toMatch(/^up to (19[.]\d\d|20[.]00) KAS$/);
    expect(r['You receive']).toMatch(/^7[.]\d+ TUSD$/);
    expect(r['Expected price (auction start)']).toBe('0.4 TUSD per KAS');
    // the worst price (all-in, the tip included) rounded against the user: fewer tokens per KAS
    expect(r['Worst price (all-in)']).toBe('0.388349 TUSD per KAS');
    // the chain's own terms stay, its prices are not said twice
    expect(r['KAS budget']).toMatch(/ KAS$/);
    expect(o.rows.filter((x) => /per TUSD/.test(x.value))).toEqual([]);
    expect(o.rows.some((x) => / \/ /.test(x.value))).toBe(false);
  });

  it('a market buy of KAS on the flipped view (a chain sell of tokens): pays the tokens, receives at least the KAS of the worst price', () => {
    const s = summarize(kob, plan(form('market', 'sell', { amount: '3' })), FIXTURE_MAKER, hint(true));
    const o = s.orders[0]!;
    expect(o.title).toBe('Buy KAS · market (auction)');
    const r = rows(o);
    expect(r['You pay']).toBe('3 TUSD');
    expect(r['You receive']).toMatch(/^at least 7[.]\d\d KAS$/);
    // a higher price in tokens per KAS is worse for a buyer of KAS: rounded up
    expect(Number(r['Worst price (all-in)']!.split(' ')[0])).toBeGreaterThan(Number(r['Expected price (auction start)']!.split(' ')[0]));
  });
});
