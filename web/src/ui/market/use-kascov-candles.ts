// kascov.io's candles of the tokens behind the chart's optional third-party line, as component state (data/kascov-candles.ts). The policy:
//   * NOTHING is requested while the line is off (`enabled` false) or the market has no tokens to ask for; turning it on asks once, turning it off or leaving the page aborts;
//   * while it is on, again only when the answer is older than `KASCOV_CANDLES_REFRESH_MS` (2 minutes) and only while the tab is visible (the timer stops when hidden; coming
//     back asks at once if the answer is stale); a failed ask is retried at the same pace, never faster;
//   * another token, network or bucket aborts the request in flight and drops what was shown: a line is never drawn for another market.
import { useEffect, useState } from 'preact/hooks';
import { useServices } from '../../app/context';
import { fetchKascovCandles, type KascovBucket, type KascovCandle } from '../../data/kascov-candles';
import { kascovNetwork } from '../../data/kascov';

export const KASCOV_CANDLES_REFRESH_MS = 120_000;

export type KascovLine =
  | { status: 'idle' }
  | { status: 'loading' }
  | { status: 'unavailable' }
  /** one candle list per asked token, in the order asked */
  | { status: 'ok'; legs: readonly (readonly KascovCandle[])[] };

type Held = { key: string; line: KascovLine };
const IDLE: KascovLine = { status: 'idle' };
const LOADING: KascovLine = { status: 'loading' };

export function useKascovCandles(o: { enabled: boolean; ids: readonly string[]; bucket: KascovBucket; fetch?: typeof fetch }): KascovLine {
  const { config } = useServices();
  const net = kascovNetwork(config.network);
  const base = config.tokenNamesUrl;
  const key = `${net}|${base}|${o.bucket}|${o.ids.join('/')}`;
  const on = o.enabled && net !== null && base !== '' && o.ids.length > 0;
  const [held, setHeld] = useState<Held | null>(null);
  const { ids, bucket } = o;
  const fetcher = o.fetch;

  useEffect(() => {
    if (!on || net === null) return;
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let ctl: AbortController | null = null;
    let lastAt = 0;
    const stop = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
    };
    const schedule = () => {
      stop();
      if (live && !document.hidden) timer = setTimeout(() => void run(), Math.max(1_000, lastAt + KASCOV_CANDLES_REFRESH_MS - Date.now()));
    };
    const run = async () => {
      stop();
      if (!live || document.hidden) return;
      const mine = new AbortController();
      ctl = mine;
      const results = await Promise.all(ids.map((id) => fetchKascovCandles(net, id, bucket, { base, signal: mine.signal, ...(fetcher ? { fetch: fetcher } : {}) })));
      if (!live || mine.signal.aborted) return;
      lastAt = Date.now();
      const legs = results.flatMap((r) => (r.ok ? [r.candles] : []));
      setHeld((prev) => (legs.length === ids.length ? { key, line: { status: 'ok', legs } } : prev && prev.key === key && prev.line.status === 'ok' ? prev : { key, line: { status: 'unavailable' } }));
      schedule();
    };
    const onVisibility = () => {
      if (document.hidden) stop();
      else if (Date.now() - lastAt >= KASCOV_CANDLES_REFRESH_MS) void run();
      else schedule();
    };
    setHeld({ key, line: LOADING });
    document.addEventListener('visibilitychange', onVisibility);
    void run();
    return () => {
      live = false;
      stop();
      ctl?.abort();
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [on, net, base, key, ids.join('/'), bucket, fetcher]);

  return !on ? IDLE : held && held.key === key ? held.line : LOADING;
}
