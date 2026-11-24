// The dust line of the ticket (dust-hint.ts) and the one flash of its "?" (HelpToggle.tsx).
import type { VNode } from 'preact';
import { describe, expect, it } from 'vitest';
import type { Intent } from '../../kob/plan';
import { DUST_LINE_SOMPI, NO_FLASH, nextDustFlash, wouldBeDust, type DustFlash } from './dust-hint';
import { HelpToggle } from './HelpToggle';

const SCALE = 1000n; // base units per whole token
const LINE = DUST_LINE_SOMPI; // 0.5 KAS
const limit = (side: 'buy' | 'sell', amount: bigint, price: bigint) => ({ type: 'limit', side, amount, price }) as unknown as Intent;

describe('wouldBeDust: one fill worth less than half the listing minimum to a taker', () => {
  it('is half the listing minimum the app knows (1 KAS)', () => {
    expect(LINE).toBe(50_000_000n);
  });

  it('a plain ask and bid: at the line not dust, just under dust, just over not', () => {
    for (const side of ['sell', 'buy'] as const) {
      // one whole token: its value is its price
      expect(wouldBeDust(limit(side, SCALE, LINE), SCALE)).toBe(false);
      expect(wouldBeDust(limit(side, SCALE, LINE - 1n), SCALE)).toBe(true);
      expect(wouldBeDust(limit(side, SCALE, LINE + 1n), SCALE)).toBe(false);
    }
  });

  it('a TWAP whose slices are under the line is dust while the whole order is far over it; a DCA the same', () => {
    const twap = { type: 'twap', amount: 1_000n * SCALE, sliceAmount: SCALE / 10n, price: 400_000_000n, interval: { minutes: 5 } } as unknown as Intent;
    // the order is worth 4,000 KAS, a slice 0.4 KAS
    expect(wouldBeDust(twap, SCALE)).toBe(true);
    expect(wouldBeDust({ ...twap, sliceAmount: SCALE } as Intent, SCALE)).toBe(false);
    expect(wouldBeDust({ ...twap, type: 'dca' } as Intent, SCALE)).toBe(true);
    // a slice auction whose end is higher is valued at it (the generous side): not dust
    expect(wouldBeDust({ ...twap, priceEnd: 600_000_000n } as Intent, SCALE)).toBe(false);
  });

  it('unsure is no: no price or amount yet, a kind that never rests, nothing at all', () => {
    expect(wouldBeDust(null, SCALE)).toBe(false);
    expect(wouldBeDust(limit('sell', 0n, LINE - 1n), SCALE)).toBe(false);
    expect(wouldBeDust({ type: 'market', side: 'buy', amount: 1n } as unknown as Intent, SCALE)).toBe(false);
    expect(wouldBeDust({ type: 'ioc', side: 'buy', amount: 1n, price: 1n } as unknown as Intent, SCALE)).toBe(false);
  });
});

describe('the "?" flashes once when the order turns dust', () => {
  const classOf = (flash: DustFlash): string => String((HelpToggle({ helpOn: true, onClick: () => undefined, flash }) as VNode<{ class: string }>).props.class);

  it('no flash class before, the flash class once it turns dust, the same while it stays dust; a new one only after it was not', () => {
    let s = NO_FLASH;
    expect(classOf(s)).toBe('tk-help-toggle');
    s = nextDustFlash(s, false);
    expect(classOf(s)).toBe('tk-help-toggle');
    s = nextDustFlash(s, true);
    const first = classOf(s);
    expect(first).toBe('tk-help-toggle tk-flash-1');
    // every keystroke while it stays dust: the same class, so the animation does not run again
    for (let k = 0; k < 5; k++) {
      const next = nextDustFlash(s, true);
      expect(next).toBe(s);
      s = next;
      expect(classOf(s)).toBe(first);
    }
    // not dust, then dust again: the other name, which restarts the animation once
    s = nextDustFlash(s, false);
    expect(classOf(s)).toBe(first);
    s = nextDustFlash(s, true);
    expect(classOf(s)).toBe('tk-help-toggle tk-flash-0');
  });
});
