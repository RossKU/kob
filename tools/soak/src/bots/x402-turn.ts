// One payer turn of the x402 bot, observable from the log alone: a line when it starts (the path the weighted pick chose), a warning
// every `warnAfterMs` while it is still running (a stalled turn is visible while it stalls, not only once it ends), and a line when it
// ends, with how long it took and whether it threw. The turn itself is never abandoned: a payment in flight keeps its own retry and
// deadline (the SDK's), so a second turn never runs beside it.
import { errText, type Logger } from '../log.ts';

export interface TurnOptions {
  /** Warn after this long, and again every time as long again (default 5 min). */
  warnAfterMs?: number;
  now?: () => number;
}

export const DEFAULT_TURN_WARN_MS = 5 * 60_000;

export async function loggedTurn<T>(log: Logger, path: string, run: () => Promise<T>, o: TurnOptions = {}): Promise<T> {
  const now = o.now ?? Date.now;
  const every = o.warnAfterMs ?? DEFAULT_TURN_WARN_MS;
  const t0 = now();
  log.info('x402 turn', { path });
  const timer = setInterval(() => log.warn('x402 turn still running', { path, ms: now() - t0 }), every);
  timer.unref?.();
  try {
    const r = await run();
    log.info('x402 turn ended', { path, ms: now() - t0 });
    return r;
  } catch (e) {
    log.warn('x402 turn ended', { path, ms: now() - t0, threw: errText(e) });
    throw e;
  } finally {
    clearInterval(timer);
  }
}
