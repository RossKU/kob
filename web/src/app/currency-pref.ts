// The viewer's display currency, KAS (the default) or USD. It is chosen under Settings, "Display
// currency", kept per browser in localStorage `kob.currency` (`usd` for USD, absent for KAS), and
// applies live without a reload. The icon choices work the same way (app/icon-pref.ts).
//
// USD is a display choice only. The order book, the ticket, the order and fill tables, the
// Activity feed and everything signed stay in KAS. No figure the user types or signs is converted
// (app/fiat.ts).
//
// The stored value is a convenience, not authority. Without storage the page works the same and
// the choice lasts until the page is closed.
import { useEffect, useState } from 'preact/hooks';

/** The localStorage key of the display currency. */
export const CURRENCY_PREF_KEY = 'kob.currency';

/** What values are shown in. */
export type DisplayCurrency = 'kas' | 'usd';

type Store = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

const listeners = new Set<(c: DisplayCurrency) => void>();
let memory: DisplayCurrency | null = null;

/** The stored choice. USD only when the stored value says so, KAS otherwise or if unreadable. */
export function readCurrencyPref(store: Store | null = browserStore()): DisplayCurrency {
  try {
    return store?.getItem(CURRENCY_PREF_KEY) === 'usd' ? 'usd' : 'kas';
  } catch {
    return 'kas';
  }
}

const current = (): DisplayCurrency => memory ?? readCurrencyPref();

/** Stores the choice and tells every open view. False when the browser refused the write. */
export function writeCurrencyPref(c: DisplayCurrency, store: Store | null = browserStore()): boolean {
  memory = c;
  for (const l of listeners) l(c);
  try {
    if (!store) return false;
    if (c === 'usd') store.setItem(CURRENCY_PREF_KEY, 'usd');
    else store.removeItem(CURRENCY_PREF_KEY);
    return true;
  } catch {
    return false;
  }
}

/** `[currency, set]`. The choice stays in step across Settings and every view that shows values. */
export function useDisplayCurrency(): [DisplayCurrency, (c: DisplayCurrency) => void] {
  const [c, setC] = useState<DisplayCurrency>(current);
  useEffect(() => {
    listeners.add(setC);
    setC(current());
    return () => void listeners.delete(setC);
  }, []);
  return [c, (v) => void writeCurrencyPref(v)];
}

/** Forgets the in-page value. For tests. */
export function resetCurrencyPrefMemory(): void {
  memory = null;
}
