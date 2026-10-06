// Every order type of the KAS ticket plans on a token/token pair, on both sides it offers (the soak's pair bots place these shapes): limit, IOC,
// FOK, market, streaming, close, TWAP, DCA, Dutch / rising, stop-market, stop-limit, trailing stop, take-profit, OCO, IFD, IFO (limit and stop
// entry), repeat IFD / IFO. Prices B base units per whole A around the pair book's mid (1475), amounts base units of A.
import { describe, expect, it } from 'vitest';
import { CLOCK, makePairEnv } from './pair-fixtures';
import { errorCodes, plan } from './pair-testkit';

const R = 1_475n;
const at = (bps: bigint): bigint => R + (R * bps) / 10_000n;
const gtdUnix = { kind: 'gtdUnix', atUnixSeconds: CLOCK.unixSeconds + 6n * 3600n };

function shapes(s: 'buy' | 'sell'): [string, unknown][] {
  const up = s === 'buy' ? 1n : -1n;
  return [
    ['limit', { type: 'limit', side: s, amount: 3_000n, price: at(-up * 200n) }],
    ['ioc', { type: 'ioc', side: s, amount: 2_000n, price: at(up * 300n) }],
    ['fok', { type: 'fok', side: s, amount: 2_000n, price: at(up * 300n) }],
    ['market', { type: 'market', side: s, amount: 2_000n }],
    ['streaming', { type: 'streaming', side: s, amount: 2_000n, displayedPrice: R, toleranceBps: 300n }],
    ['dutch', { type: 'dutch', side: s, amount: 3_000n, price: at(-up * 200n), priceEnd: at(up * 40n), duration: { seconds: 600n } }],
    ['stopMarket', { type: 'stopMarket', side: s, amount: 3_000n, stop: at(up * 300n), expiry: gtdUnix }],
    ['stopLimit', { type: 'stopLimit', side: s, amount: 3_000n, stop: at(up * 300n), limit: at(up * 400n), expiry: gtdUnix }],
    ['trailingStop', { type: 'trailingStop', side: s, amount: 3_000n, stop: at(up * 300n), trail: { step: 15n, gap: 45n, wait: 600n, expectedUpdates: 10 }, expiry: gtdUnix }],
    ['takeProfit', { type: 'takeProfit', side: s, amount: 3_000n, price: at(-up * 800n) }],
    ['oco', { type: 'oco', side: s, amount: 3_000n, takeProfit: at(-up * 800n), stop: at(up * 300n) }],
    ['ifd', { type: 'ifd', side: s, amount: 3_000n, entry: { price: at(-up * 200n) }, exit: { takeProfit: at(up * 600n) } }],
    ['ifo', { type: 'ifo', side: s, amount: 3_000n, entry: { price: at(-up * 200n) }, exit: { takeProfit: at(up * 600n), stop: at(-up * 400n) } }],
    ['ifoStopEntry', { type: 'ifo', side: s, amount: 3_000n, entry: { stop: at(up * 200n), price: at(up * 900n) }, exit: { takeProfit: at(up * 1_800n), stop: at(-up * 400n) } }],
    ['repeatIfd', { type: 'repeatIfd', side: s, amount: 3_000n, entry: { price: at(-up * 200n) }, exit: { takeProfit: at(up * 400n) }, repeat: { count: 2n } }],
    ['repeatIfo', { type: 'repeatIfo', side: s, amount: 3_000n, entry: { price: at(-up * 200n) }, exit: { takeProfit: at(up * 400n), stop: at(-up * 1_500n) }, repeat: { count: 2n } }],
  ];
}

describe('every KAS-ticket order type plans on a pair', () => {
  for (const s of ['buy', 'sell'] as const) {
    for (const [name, intent] of shapes(s)) {
      it(`${name} ${s}`, () => {
        const p = plan(makePairEnv(), intent);
        expect(errorCodes(p), JSON.stringify(p.issues.map((i) => [i.code, i.message]))).toEqual([]);
        expect(p.ok && p.built !== null && p.pair !== null).toBe(true);
        const kind = p.pair!.kind;
        expect(kind).toBe(/^(limit|ioc|fok|market|streaming|dutch)$/.test(name) ? 'KobPair' : /^(ifd|ifo|repeat)/.test(name) ? 'KobIfdPair' : 'KobCondPair');
      });
    }
  }
  it('TWAP sells, DCA buys and close sells the held A, in slices / at market', () => {
    const env = makePairEnv();
    for (const intent of [
      { type: 'twap', side: 'sell', amount: 6_000n, sliceAmount: 2_000n, interval: { seconds: 120n }, price: at(-50n) },
      { type: 'dca', side: 'buy', amount: 6_000n, sliceAmount: 2_000n, interval: { seconds: 120n }, price: at(50n), maxFills: 6n },
      { type: 'close', amount: 2_000n },
    ]) {
      const p = plan(env, intent);
      expect(errorCodes(p), `${intent.type}: ${JSON.stringify(p.issues.map((i) => i.code))}`).toEqual([]);
      expect(p.pair?.kind).toBe('KobPair');
    }
  });
});
