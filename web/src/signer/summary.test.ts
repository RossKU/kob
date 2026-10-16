// The signer frame's own summary must show what the user agrees to beyond the net effect: two requests that differ only in an order's price (or
// stop) move the same KAS and tokens, so a summary of the net effect alone shows them identically. The frame shows it as one sentence (`text`).
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
    // the one sentence the frame shows carries the price too (in KAS per whole token, the ticket's words)
    expect(fair.text).toMatch(/^Sell 10 EXKCC at 2.5 KAS each./);
    expect(cheap.text).toMatch(/^Sell 10 EXKCC at 0.0025 KAS each./);
    expect(cheap.text).not.toBe(fair.text);
  });

  it('is one sentence in the words of the ticket: what is sold at what price, what it locks, the fee, the token by its covenant id', () => {
    const s = summarize(kob, build('create.ask'), MAKER_PK, HINTS);
    expect(s.text.startsWith('Sell 10 EXKCC at 2.5 KAS each. Locks 20.00 KAS and 10 EXKCC (unused part returns); fee 0.008 KAS. Token ids: EXKCC ')).toBe(true);
    expect(s.text).toContain(`${TOKEN_COV_ID.slice(0, 8)}…${TOKEN_COV_ID.slice(-8)}`);
    // a buy: the amount its escrow buys at its limit (not the KAS budget the chain holds)
    const bid = summarize(kob, build('create.bid'), MAKER_PK, HINTS).text;
    expect(bid).toMatch(/^Buy [0-9.,]+ EXKCC at 2[.]45 KAS each[.] Locks [0-9.,]+ KAS [(]unused part returns[)]/);
    // the market shown inverted (KAS/EXKCC): the same sale is a purchase of KAS at EXKCC per KAS
    const inv = summarize(kob, build('create.ask'), MAKER_PK, [{ ...HINTS[0]!, inverted: true }]);
    expect(inv.text.startsWith('Buy 24.99 KAS at 0.4 EXKCC each. Locks 20.00 KAS and 10 EXKCC')).toBe(true); // what the sale receives: the price less the tip
    // a cancel creates no order: the kind of request leads
    const c = summarize(kob, kob.build(goldenRequest('cancel.ask', MAKER_PK)), MAKER_PK, HINTS);
    expect(c.text.startsWith('Cancel order. ')).toBe(true);
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
    expect(b.text).not.toBe(a.text);
    expect(a.text).toContain('stop ');
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
  it('states the scale of the price next to the base-unit amount (TN10 2026-10-15 B-13)', () => {
    const s = summarize(kob, build('create.ask'), MAKER_PK, []);
    const rows = s.orders[0]!.rows;
    expect(rows.find((r) => /amount/i.test(r.label))?.value).toMatch(/base units/);
    const price = rows.find((r) => /limit price/i.test(r.label))?.value ?? '';
    expect(price).toMatch(/KAS per \d+ base units of the token$/);
    // with the decimals known the price stays per whole token
    const known = summarize(kob, build('create.ask'), MAKER_PK, HINTS).orders[0]!.rows.find((r) => /limit price/i.test(r.label))?.value ?? '';
    expect(known).not.toMatch(/base units/);
  });
});

describe('signer summary: a market buy typed in KAS on the inverted view', () => {
  it('says the KAS typed as the most it spends, what it buys about and at least (TN10 2026-10-17: "Sell 20.60 KAS" for 20 typed)', () => {
    const f = { ...form('market', 'buy', { amount: '20' }), kas: true };
    const plan = planOrder(ticketEnv(), buildIntent(f, { ...CTX, ref: { buy: 250_000_000n, sell: 245_000_000n } }).intent!);
    const s = summarize(kob, plan.built!, FIXTURE_MAKER, [{ covenantId: TOKEN_COV, ticker: 'EXKCC', decimals: 8, inverted: true }]);
    expect(s.text).toMatch(/^Sell up to (19[.]\d\d|20[.]00) KAS for about [0-9.]+ EXKCC [(]market, at least [0-9.]+ EXKCC[)][.]/);
  });
});
