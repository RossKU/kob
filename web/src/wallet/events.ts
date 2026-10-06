// Shared plumbing of the adapters' `subscribe`: provider event emitters (`on` / `removeListener`) and the visible-tab poll used when a
// provider has none. Pure apart from `setInterval` / `document.visibilityState` (both guarded).
import type { WalletEvent } from './types';

/** The EventEmitter subset all three wallet providers expose (verified in the extensions' injected scripts: `on`, `removeListener`). */
export interface EmitterLike {
  on?: (event: string, handler: (...args: any[]) => void) => unknown;
  removeListener?: (event: string, handler: (...args: any[]) => void) => unknown;
  off?: (event: string, handler: (...args: any[]) => void) => unknown;
}

export const hasEmitter = (p: EmitterLike | null | undefined): p is Required<Pick<EmitterLike, 'on'>> & EmitterLike =>
  !!p && typeof p.on === 'function' && (typeof p.removeListener === 'function' || typeof p.off === 'function');

/** Provider event name -> WalletEvent. `accounts` and `network` handlers get the raw handler arguments. */
export interface EventMap {
  accounts: string[];
  network: string[];
  disconnect?: string[];
}

const asAccounts = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : typeof v === 'string' ? [v] : []);

/**
 * Registers `handler`s on the provider's emitter and returns an idempotent remover. Every handler is registered once per name, so a
 * second `subscribe` on the same provider adds its own pair and its remover takes out exactly that pair.
 */
export function listenProvider(p: EmitterLike, names: EventMap, emit: (e: WalletEvent) => void): () => void {
  const off = (event: string, h: (...a: any[]) => void) => (typeof p.removeListener === 'function' ? p.removeListener(event, h) : p.off?.(event, h));
  const reg: [string, (...a: any[]) => void][] = [];
  const add = (event: string, h: (...a: any[]) => void) => {
    p.on!(event, h);
    reg.push([event, h]);
  };
  let active = true;
  const guard = <A extends unknown[]>(f: (...a: A) => void) => (...a: A) => {
    if (active) f(...a);
  };
  for (const n of names.accounts) add(n, guard((a: unknown) => emit({ kind: 'accounts', accounts: asAccounts(a) })));
  for (const n of names.network) add(n, guard((n2: unknown) => emit({ kind: 'network', network: typeof n2 === 'string' ? n2 : (n2 as { network?: string } | null)?.network ?? '' })));
  for (const n of names.disconnect ?? []) add(n, guard(() => emit({ kind: 'disconnect' })));
  return () => {
    if (!active) return;
    active = false;
    for (const [event, h] of reg) {
      try {
        off(event, h);
      } catch {
        /* a provider that vanished has nothing left to remove */
      }
    }
    reg.length = 0;
  };
}

export const DEFAULT_POLL_MS = 4000;

/**
 * Fallback for a provider without events: reads `{account, network}` every `pollMs` while the tab is visible and reports differences.
 * The first read is the baseline (the caller has just connected), so nothing is emitted for the state the app already knows.
 */
export function pollProvider(read: () => Promise<{ account: string; network: string }>, emit: (e: WalletEvent) => void, pollMs = DEFAULT_POLL_MS): () => void {
  let active = true;
  let last: { account: string; network: string } | null = null;
  let busy = false;
  const tick = async () => {
    if (!active || busy) return;
    if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return;
    busy = true;
    try {
      const now = await read();
      if (!active) return;
      if (last !== null) {
        if (now.account !== last.account) emit({ kind: 'accounts', accounts: now.account ? [now.account] : [] });
        if (now.network !== last.network) emit({ kind: 'network', network: now.network });
      }
      last = now;
    } catch {
      /* a wallet that cannot answer right now is retried on the next tick; the app re-reads on every event anyway */
    } finally {
      busy = false;
    }
  };
  void tick();
  const id = setInterval(() => void tick(), pollMs);
  return () => {
    if (!active) return;
    active = false;
    clearInterval(id);
  };
}
