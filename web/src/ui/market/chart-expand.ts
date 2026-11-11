// The "Expand chart" choice of the market pages (token and pair), kept in this browser. One flag for every market: the price chart's button flips it, the page's grid
// (MarketShell, market.css .mkt-chart-big) lays out accordingly. Three columns: the chart takes the book's and the depth chart's place; two columns and a phone: a taller
// chart box. A store of its own so the button (inside the chart) and the grid (around it) agree without one passing props through the other.
import { useEffect, useState } from 'preact/hooks';

const KEY = 'kob.chart.expand';

function read(): boolean {
  try {
    return localStorage.getItem(KEY) === '1';
  } catch {
    return false; // no storage: the chart is not expanded
  }
}

let current = read();
const listeners = new Set<(on: boolean) => void>();

/** Expand or restore the chart; the choice is remembered (not when there is no storage: then it lasts until the page is left). */
export function setChartExpanded(on: boolean): void {
  if (on === current) return;
  current = on;
  try {
    if (on) localStorage.setItem(KEY, '1');
    else localStorage.removeItem(KEY);
  } catch {
    /* not kept */
  }
  listeners.forEach((l) => l(on));
}

/** Whether the chart is expanded now; follows a change made anywhere on the page. */
export function useChartExpanded(): boolean {
  const [on, setOn] = useState(current);
  useEffect(() => {
    listeners.add(setOn);
    setOn(current);
    return () => void listeners.delete(setOn);
  }, []);
  return on;
}
