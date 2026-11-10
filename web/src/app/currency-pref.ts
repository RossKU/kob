// The viewer's display currency (Settings, "Display currency"): KAS (the default) or USD, per browser, live (no reload), kept in localStorage `kob.currency`
// (`usd` when USD, absent for KAS), like the icon choices (app/icon-pref.ts). USD is a DISPLAY choice: the order book, the ticket, the order and fill tables,
// the Activity feed and everything signed stay in KAS, and no figure the user types or signs is ever converted (app/fiat.ts).
//
// Not authority: the page works the same without storage (the choice then lasts until the page is closed).
import { useEffect, useState } from 'preact/hooks';

export const CURRENCY_PREF_KEY = 'kob.currency';

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

/** The stored choice: USD only when the stored value says so, KAS for anything else and for an unreadable store. */
export function readCurrencyPref(store: Store | null = browserStore()): DisplayCurrency {
  try {
    return store?.getItem(CURRENCY_PREF_KEY) === 'usd' ? 'usd' : 'kas';
  } catch {
    return 'kas';
  }
}

const current = (): DisplayCurrency => memory ?? readCurrencyPref();

/** Stores the choice and tells every open view; false when the browser refused the write (the choice then lasts until the page is closed). */
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

/** `[currency, set]`: the choice, kept in step across the Settings page and every view that shows values. */
export function useDisplayCurrency(): [DisplayCurrency, (c: DisplayCurrency) => void] {
  const [c, setC] = useState<DisplayCurrency>(current);
  useEffect(() => {
    listeners.add(setC);
    setC(current());
    return () => void listeners.delete(setC);
  }, []);
  return [c, (v) => void writeCurrencyPref(v)];
}

/** Forgets the in-page value (tests). */
export function resetCurrencyPrefMemory(): void {
  memory = null;
}
