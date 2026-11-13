// Wallet discovery. Browser extensions inject their provider after the page loads, and sometimes
// after `DOMContentLoaded`, so a single check at start-up misses them. `watchWallets` polls for a
// while and also reacts to Kaspire's `kaspire#initialized` event. Kastle is only offered when
// `features.kastle` is on.
import type { WalletAdapter, WalletId } from './types';
import { createKaswareAdapter, type KaswareProvider } from './kasware';
import { createKaspireAdapter, type KaspireDeps, type KaspireProvider } from './kaspire';
import { createKastleAdapter, type KastleProvider } from './kastle';

/** The part of `window` the wallets live on. It can be injected for tests. */
export interface WalletHost {
  kasware?: KaswareProvider;
  kaspire?: KaspireProvider;
  kastle?: KastleProvider;
  addEventListener?(type: string, cb: () => void): void;
  removeEventListener?(type: string, cb: () => void): void;
}

/** Options of the wallet discovery functions. */
export interface DiscoverOptions {
  /** The host to read. Default `window`, or `globalThis`. */
  host?: WalletHost;
  /** Offer Kastle (`features.kastle`). */
  kastle?: boolean;
  intervalMs?: number;
  /** Stop polling after this long, in ms. Default 5 s, and 0 polls until stopped. */
  timeoutMs?: number;
  kaspire?: Pick<KaspireDeps, 'dispatchTag' | 'mode'>;
}

const defaultHost = (): WalletHost => (typeof window !== 'undefined' ? (window as unknown as WalletHost) : (globalThis as unknown as WalletHost));

/** Adapters bound to a host. All of them are returned, and `detect()` says which are installed. */
export function createAdapters(o: Pick<DiscoverOptions, 'host' | 'kastle' | 'kaspire'> = {}): WalletAdapter[] {
  const host = o.host ?? defaultHost();
  const list = [
    createKaswareAdapter({ getProvider: () => (host.kasware ?? null) }),
    createKaspireAdapter({ getProvider: () => (host.kaspire ?? null), ...o.kaspire }),
  ];
  if (o.kastle) list.push(createKastleAdapter({ getProvider: () => (host.kastle ?? null) }));
  return list;
}

/**
 * Calls `onChange` with the detected adapters whenever that set changes, the first time as soon as
 * one is found. Returns a stop function. Polling ends after `timeoutMs`. Late injections after
 * that are caught by the `kaspire#initialized` listener until stopped.
 */
export function watchWallets(onChange: (detected: WalletAdapter[]) => void, o: DiscoverOptions = {}): () => void {
  const host = o.host ?? defaultHost();
  const adapters = createAdapters({ ...o, host });
  let last: string | null = null;
  let stopped = false;
  const check = () => {
    if (stopped) return;
    const detected = adapters.filter((a) => a.detect());
    const key = detected.map((a) => a.id).join(',');
    if (key !== last) {
      last = key;
      onChange(detected);
    }
  };
  const onEvent = () => check();
  host.addEventListener?.('kaspire#initialized', onEvent);
  host.addEventListener?.('kasware#initialized', onEvent);
  host.addEventListener?.('kastle#initialized', onEvent);
  check();
  const interval = setInterval(check, o.intervalMs ?? 250);
  const timeoutMs = o.timeoutMs ?? 5000;
  const end = timeoutMs > 0 ? setTimeout(() => clearInterval(interval), timeoutMs) : null;
  return () => {
    stopped = true;
    clearInterval(interval);
    if (end) clearTimeout(end);
    host.removeEventListener?.('kaspire#initialized', onEvent);
    host.removeEventListener?.('kasware#initialized', onEvent);
    host.removeEventListener?.('kastle#initialized', onEvent);
  };
}

/** Options of `discoverWallets`. */
export interface DiscoverWalletsOptions extends DiscoverOptions {
  /** After the first wallet shows up, wait this long for more, in ms. Default 300. */
  settleMs?: number;
}

/**
 * Resolves with the installed wallets. That is as soon as one is found, plus a short settle time
 * for a second one, or `[]` when none appears within `timeoutMs` (default 2.5 s).
 */
export function discoverWallets(o: DiscoverWalletsOptions = {}): Promise<WalletAdapter[]> {
  return new Promise((resolve) => {
    let latest: WalletAdapter[] = [];
    let settle: ReturnType<typeof setTimeout> | null = null;
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      if (settle) clearTimeout(settle);
      clearTimeout(giveUp);
      stop();
      resolve(latest);
    };
    const stop = watchWallets(
      (detected) => {
        latest = detected;
        if (detected.length && !settle) settle = setTimeout(finish, o.settleMs ?? 300);
      },
      { ...o, timeoutMs: 0 },
    );
    const giveUp = setTimeout(finish, o.timeoutMs ?? 2500);
  });
}

/** The ids of the extension wallets, in the order they are offered. */
export const WALLET_IDS: readonly WalletId[] = ['kasware', 'kaspire', 'kastle'];
