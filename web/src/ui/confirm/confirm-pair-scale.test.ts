// A pair order's state price is B base units per scale(A) base units of A, scale(A) = 10^min(decimals(A), 9). The pre-sign screen shows it per
// WHOLE A, like the ticket's disclosure: for every decimals of A (0..18) both show the same "150 BBB / AAA" for the same planned state.
import { describe, expect, it } from 'vitest';
import { makePairEnv, level } from '../../kob/orders/pair-fixtures';
import { pairValid, K } from '../../kob/orders/pair-testkit';
import { market3x3, market8x8 } from '../../testing/fixtures';
import { describeOrder } from '../../kob/order-facts';
import { pow10 } from '../../kob/units';
import { buildDisclosureModel } from '../ticket/disclosure-model';
import { t } from '../../i18n';
import { orderRows } from './confirm-model';

const B_COV = '71'.repeat(32);
const B_DECIMALS = 8;
/** 150 BBB per whole AAA, in B base units */
const PER_WHOLE_A = 150n * pow10(B_DECIMALS);
const text = (v: unknown): string => (typeof v === 'string' ? v : JSON.stringify(v));

describe('pair order price on the ticket and on the confirmation, decimals of A 0..18', () => {
  for (let decimals = 0; decimals <= 18; decimals++) {
    it(`decimals(A) = ${decimals}`, () => {
      const a = market3x3({ ticker: 'AAA', decimals, tick: 1 });
      const b = market8x8({ covenant_id: B_COV, ticker: 'BBB', decimals: B_DECIMALS });
      expect(a.scale).toBe(pow10(Math.min(decimals, 9)));
      const whole = pow10(decimals);
      const price = (PER_WHOLE_A * a.scale) / whole; // the state price: per scale(A) base units
      expect((price * whole) / a.scale).toBe(PER_WHOLE_A);
      const book = { asks: [level(price + price / 10n, 3n * whole, 1)], bids: [level(price - price / 10n, 3n * whole, 1)] };
      const env = makePairEnv({ a, b, book, aAmounts: [3n * whole], kasPerWholeA: 3n * 100_000_000n, kasPerWholeB: 2_000_000n });
      const p = pairValid(env, { type: 'limit', side: 'sell', amount: 2n * whole, price });

      const disc = buildDisclosureModel(p, { ticker: 'AAA', decimals, scale: a.scale, clock: env.clock, quote: { ticker: 'BBB', decimals: B_DECIMALS } })!;
      const ticket = text(disc.price.find((r) => r.id === 'limit')!.value);

      const d = describeOrder(p.states[0]!, K);
      const registry = { byCovenantId: new Map([[B_COV, { ticker: 'BBB', decimals: B_DECIMALS, tradable: true }]]) };
      const ref = { covenantId: a.covenantId, ticker: 'AAA', decimals, display: '', inRegistry: true, tradable: true };
      const rows = orderRows(d, { value: null, locked: null, deadline: null }, { tr: t, ref, clock: null, time: (u: bigint) => u.toString(), registry } as never);
      const confirm = rows.find((r) => r.id === 'price')!.value;

      expect(ticket).toBe('150 BBB / AAA');
      expect(confirm).toBe(ticket);
      expect(rows.find((r) => r.id === 'pairTotal')!.value).toBe('300 BBB');
    });
  }
});
