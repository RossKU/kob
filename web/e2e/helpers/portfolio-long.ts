// Long values for the one-line tables of the Portfolio (the widest texts the lists show). Mock stack.
import type { MockClient } from '../fixtures';
import { PF_ID } from './portfolio-seed';

/**
 * A hex-named token's order of 1,960.83737795 tokens filled whole (its Filled cell reads "1,960.83737795 / 1,960.83737795 <long name> 100%"), and a sell that
 * ended "expired (IOC/FOK)". With the browser in a time zone where it is 10-11 PM (`lateTimeZone`) the dates read like "11/10/26, 10:38 PM", the longest a date gets.
 */
export async function seedLongValues(mock: MockClient): Promise<void> {
  await mock.seed({
    tokens: [{ ticker: '', covenant_id: PF_ID, program: 'KCC20Ref', template_id: 'KCC20Ref', extension_commitment: 'ee'.repeat(32), powers: [], decimals: null, order_scale: 100_000_000 }],
    orders: [
      { token: PF_ID, side: 'ask', maker: 'alice', amount: '196083737795', price: 4_990_000, history: 'filled', ago_daa: 5000 },
      { token: PF_ID, side: 'ask', maker: 'alice', amount: '1125193602', price: 3_437_005_454, history: 'killed', ago_daa: 3000 },
    ],
  });
}

/** A time zone in which it is 10-11 PM now, so every date a page shows reads with a two-digit hour and PM ("11/10/26, 10:38 PM"). */
export function lateTimeZone(): string {
  const utcHour = new Date().getUTCHours();
  let k = (22 - utcHour + 24) % 24; // hours east of UTC
  if (k > 14) k -= 24;
  return k === 0 ? 'UTC' : k > 0 ? `Etc/GMT-${k}` : `Etc/GMT+${-k}`;
}
