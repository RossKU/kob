import { describe, expect, it } from 'vitest';
import { crosshairLabel, tickLabel, TICK } from './chart-time';

const L = 'en-US';
// 2026-11-11 00:00 UTC (= 09:00 in Tokyo, 19:00 on Nov 10 in New York) and a day later
const D0 = Date.UTC(2026, 10, 11) / 1000;
const DAY = 86_400;

describe('the time axis labels', () => {
  it('a daily chart reads a date at every tick, whatever the zone and the tick kind (never the 09:00 of Tokyo)', () => {
    const labels = [0, 1, 2, 3].map((i) => tickLabel(D0 + i * DAY, '1d', TICK.day, L, 'Asia/Tokyo'));
    expect(labels).toEqual(['Nov 11', 'Nov 12', 'Nov 13', 'Nov 14']);
    // a time-kind tick on a daily chart is still a date; the zone does not move a daily bar to the evening before
    expect(tickLabel(D0, '1d', TICK.time, L, 'Asia/Tokyo')).toBe('Nov 11');
    expect(tickLabel(D0, '1d', TICK.time, L, 'America/New_York')).toBe('Nov 11');
    expect(new Set(labels).size).toBe(labels.length);
  });

  it('boundaries: the month, the year', () => {
    expect(tickLabel(Date.UTC(2026, 11, 1) / 1000, '1d', TICK.month, L)).toBe('Dec');
    expect(tickLabel(Date.UTC(2027, 0, 1) / 1000, '1d', TICK.year, L)).toBe('2027');
    expect(tickLabel(Date.UTC(2027, 0, 1) / 1000, '1h', TICK.year, L, 'UTC')).toBe('2027');
  });

  it('intraday ticks are 24-hour clock times in the viewer zone, and midnight of that zone is the date', () => {
    expect(tickLabel(D0 + 5 * 3600, '1h', TICK.time, L, 'UTC')).toBe('05:00');
    expect(tickLabel(D0 + 14 * 3600 + 35 * 60, '5m', TICK.time, L, 'UTC')).toBe('14:35');
    expect(tickLabel(D0 + 14 * 3600 + 35 * 60, '5m', TICK.time, L, 'Asia/Tokyo')).toBe('23:35');
    expect(tickLabel(D0 - 9 * 3600, '1h', TICK.time, L, 'Asia/Tokyo')).toBe('Nov 11'); // 00:00 in Tokyo, never "24:00"
    expect(tickLabel(D0, '1m', TICK.day, L, 'UTC')).toBe('Nov 11');
    for (const h of [0, 1, 11, 12, 13, 23]) expect(tickLabel(D0 + h * 3600 + 60, '1h', TICK.time, L, 'UTC')).not.toMatch(/AM|PM/);
  });

  it('the crosshair label: the date, and the 24-hour time for an intraday bar', () => {
    expect(crosshairLabel(D0, '1d', L, 'Asia/Tokyo')).toBe('Nov 11, 2026');
    expect(crosshairLabel(D0 + 14 * 3600 + 35 * 60, '5m', L, 'UTC')).toBe('Nov 11, 2026, 14:35');
    expect(crosshairLabel(D0, '1h', L, 'UTC')).toBe('Nov 11, 2026, 00:00');
  });
});
