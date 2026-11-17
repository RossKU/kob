// The wallet's delivery settings for buy orders (kob/orders/delivery.ts, bid-delivery-spec §10), kept per browser in localStorage:
// - `kob.delivery`: the default delivery of buy orders (`collected`, `topup`; absent: Each fill). Chosen under Settings, applies at once; the ticket's
//   Advanced options can still pick another one per order.
// - `kob.delivery.threshold.<wallet key>.<token covenant id>.<extension commitment>`: the receive threshold of Top-up for that token, set at the
//   wallet's first top-up order of it and reused by every later one. It changes only when the user resets it (Settings).
//
// The stored values are a convenience, not authority: without storage the page works the same (Each fill, and a threshold from each order's
// price) and a choice lasts until the page is closed.
import { useEffect, useState } from 'preact/hooks';
import { isDeliveryChoice, type DeliveryChoice } from '../../kob/orders/delivery';

/** The localStorage key of the default delivery. */
export const DELIVERY_PREF_KEY = 'kob.delivery';
/** The prefix of the stored receive thresholds. */
export const THRESHOLD_KEY_PREFIX = 'kob.delivery.threshold.';

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
type ListStore = Store & Pick<Storage, 'length' | 'key'>;
const browserStore = (): ListStore | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

const listeners = new Set<(c: DeliveryChoice) => void>();
let memory: DeliveryChoice | null = null;

/** The stored default: Collected or Top-up when the stored value says so, Each fill otherwise or if unreadable. */
export function readDeliveryPref(store: Store | null = browserStore()): DeliveryChoice {
  try {
    const v = store?.getItem(DELIVERY_PREF_KEY);
    return isDeliveryChoice(v) ? v : 'each';
  } catch {
    return 'each';
  }
}

const current = (): DeliveryChoice => memory ?? readDeliveryPref();

/** Stores the default and tells every open view. False when the browser refused the write. */
export function writeDeliveryPref(c: DeliveryChoice, store: Store | null = browserStore()): boolean {
  memory = c;
  for (const l of listeners) l(c);
  try {
    if (!store) return false;
    if (c === 'each') store.removeItem(DELIVERY_PREF_KEY);
    else store.setItem(DELIVERY_PREF_KEY, c);
    return true;
  } catch {
    return false;
  }
}

/** `[default, set]`, in step across Settings and every open ticket. */
export function useDeliveryPref(): [DeliveryChoice, (c: DeliveryChoice) => void] {
  const [c, setC] = useState<DeliveryChoice>(current);
  useEffect(() => {
    listeners.add(setC);
    setC(current());
    return () => void listeners.delete(setC);
  }, []);
  return [c, (v) => void writeDeliveryPref(v)];
}

/** Forgets the in-page value. For tests. */
export function resetDeliveryPrefMemory(): void {
  memory = null;
}

// ------------------------------------------------------------------------------------------------ receive thresholds

/** What a stored threshold is for. */
export interface ThresholdKey {
  maker: string;
  tokenCovId: string;
  extensionCommitment: string;
}
/** A stored threshold with what Settings shows of its token. */
export interface StoredThreshold extends ThresholdKey {
  /** base units of the token */
  threshold: bigint;
  ticker: string;
  decimals: number;
}

const HEX = /^[0-9a-f]+$/;
export const thresholdKey = (k: ThresholdKey): string => `${THRESHOLD_KEY_PREFIX}${k.maker}.${k.tokenCovId}.${k.extensionCommitment}`;

function parse(key: string, raw: string | null): StoredThreshold | null {
  if (!raw || !key.startsWith(THRESHOLD_KEY_PREFIX)) return null;
  const parts = key.slice(THRESHOLD_KEY_PREFIX.length).split('.');
  if (parts.length !== 3 || !parts.every((p) => HEX.test(p))) return null;
  try {
    const v = JSON.parse(raw) as { threshold?: unknown; ticker?: unknown; decimals?: unknown };
    if (typeof v.threshold !== 'string' || !/^[1-9][0-9]*$/.test(v.threshold)) return null;
    const decimals = typeof v.decimals === 'number' && Number.isInteger(v.decimals) && v.decimals >= 0 && v.decimals <= 18 ? v.decimals : 0;
    return { maker: parts[0]!, tokenCovId: parts[1]!, extensionCommitment: parts[2]!, threshold: BigInt(v.threshold), ticker: typeof v.ticker === 'string' ? v.ticker : '', decimals };
  } catch {
    return null;
  }
}

/** The stored threshold of a token for this wallet, or undefined (none yet, unreadable storage). */
export function readThreshold(k: ThresholdKey, store: Store | null = browserStore()): bigint | undefined {
  try {
    const key = thresholdKey(k);
    return parse(key, store?.getItem(key) ?? null)?.threshold;
  } catch {
    return undefined;
  }
}

/**
 * Stores the threshold of the wallet's first top-up order of a token. An existing value is kept (it changes only by a reset). True when this
 * call stored it.
 */
export function rememberThreshold(t: StoredThreshold, store: Store | null = browserStore()): boolean {
  if (t.threshold <= 0n) return false;
  try {
    if (!store || readThreshold(t, store) !== undefined) return false;
    store.setItem(thresholdKey(t), JSON.stringify({ threshold: t.threshold.toString(), ticker: t.ticker, decimals: t.decimals }));
    return true;
  } catch {
    return false;
  }
}

/** Forgets a stored threshold: the next top-up order of the token sets it again from its price. */
export function resetThreshold(k: ThresholdKey, store: Store | null = browserStore()): void {
  try {
    store?.removeItem(thresholdKey(k));
  } catch {
    // nothing stored, nothing to forget
  }
}

/** Every threshold stored for a wallet key, by ticker. */
export function listThresholds(maker: string, store: ListStore | null = browserStore()): StoredThreshold[] {
  const out: StoredThreshold[] = [];
  try {
    if (!store) return out;
    for (let i = 0; i < store.length; i++) {
      const key = store.key(i);
      if (key === null || !key.startsWith(`${THRESHOLD_KEY_PREFIX}${maker}.`)) continue;
      const v = parse(key, store.getItem(key));
      if (v) out.push(v);
    }
  } catch {
    return out;
  }
  return out.sort((a, b) => a.ticker.localeCompare(b.ticker) || a.tokenCovId.localeCompare(b.tokenCovId));
}
