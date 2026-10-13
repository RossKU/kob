// The signer frame's own summary must show what the user agrees to beyond the net effect: two requests that differ only in an order's price (or
// stop) move the same KAS and tokens, so a summary of the net effect alone shows them identically.
import { describe, expect, it } from 'vitest';
import { summarize } from './summary';
import { goldenRequest } from '../testing/golden';
import { goldenRequest as chainGoldenRequest } from '../testing/chain-fixtures';
import { MAKER_PK, TOKEN_COV_ID } from '../testing/token-fixtures';
import { kob } from '../testing/wallet-fixtures';
import type { ActionRequest, BuiltTx } from '../kob/types';

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
