// The labels of the price chart's time axis and of its crosshair (Lightweight Charts
// `timeScale.tickMarkFormatter` and `localization.timeFormatter`). They are pure functions of the
// bar's time, the interval and the tick's kind, so they are unit-tested (chart-time.test.ts)
// without the library. PriceChart.tsx wires them in.
//
// What the axis says:
// - A day-level tick is a date ("Nov 11").
// - An intraday tick is the 24-hour time ("14:35", never "02:35 PM").
// - A month boundary is the month ("Nov").
// - A year boundary is the year ("2027").
// The library tells each tick's kind, its weight: a year, a month, a day or a time of day. The
// label follows the kind, so a row of day-level ticks of the 1D chart can never read as one and
// the same time of day. The bars of a daily chart all start at 00:00 UTC, which is 09:00 in
// Tokyo, so a time under every tick says nothing.
//
// The zone: intraday bars are read in the viewer's own zone, which is what their clock says, with
// `timeZone` left out. A daily bar is a UTC day. Its bucket starts at 00:00 UTC, which is the
// previous evening west of Greenwich. Its label is that day's date, so the 1D axis is read in UTC
// whatever the viewer's zone. Otherwise a New York viewer would see every daily bar one day early.
import type { CandleInterval } from '../../data/indexer-types';

/**
 * Lightweight Charts' `TickMarkType` values. The library is loaded on demand, so its enum is not
 * imported here.
 */
export const TICK = { year: 0, month: 1, day: 2, time: 3, timeSeconds: 4 } as const;

const zoneOf = (interval: CandleInterval, timeZone: string | undefined): string | undefined => (interval === '1d' ? 'UTC' : timeZone);
const format = (locale: string, o: Intl.DateTimeFormatOptions, timeZone: string | undefined, sec: number): string =>
  new Intl.DateTimeFormat(locale, timeZone ? { ...o, timeZone } : o).format(new Date(sec * 1000));
// `hourCycle: 'h23'` and not `hour12: false`, because the latter reads midnight as "24:00" in some
// engines.
const CLOCK: Intl.DateTimeFormatOptions = { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' };

/**
 * The label of one tick of the time axis. `type` is the library's tick kind (`TICK`). `timeZone` is
 * for tests and defaults to the viewer's zone.
 */
export function tickLabel(sec: number, interval: CandleInterval, type: number, locale: string, timeZone?: string): string {
  const zone = zoneOf(interval, timeZone);
  if (type === TICK.year) return format(locale, { year: 'numeric' }, zone, sec);
  if (type === TICK.month) return format(locale, { month: 'short' }, zone, sec);
  const date = () => format(locale, { month: 'short', day: 'numeric' }, zone, sec);
  // Every tick of a daily chart is a day, never a time of day. An intraday tick that falls on the
  // viewer's midnight is the date of the day it starts.
  if (interval === '1d' || type === TICK.day) return date();
  const clock = format(locale, CLOCK, zone, sec);
  return clock === '00:00' ? date() : clock;
}

/**
 * The label of the crosshair on the time axis. It is the kit's one time form (ui/kit/format.ts
 * `formatRowTime`): the year-first date `YYYY-MM-DD` and, for an intraday bar, the 24-hour time
 * after it (`YYYY-MM-DD HH:MM`). The axis ticks stay short (`Nov 11`, `14:35`).
 */
export function crosshairLabel(sec: number, interval: CandleInterval, _locale: string, timeZone?: string): string {
  const zone = zoneOf(interval, timeZone);
  const parts = Object.fromEntries(
    new Intl.DateTimeFormat('en-US', { year: 'numeric', month: '2-digit', day: '2-digit', ...(interval === '1d' ? {} : CLOCK), ...(zone ? { timeZone: zone } : {}) })
      .formatToParts(new Date(sec * 1000))
      .map((p) => [p.type, p.value]),
  );
  const date = `${parts.year}-${parts.month}-${parts.day}`;
  return interval === '1d' ? date : `${date} ${parts.hour}:${parts.minute}`;
}
