// Which creator-hosted (https) logos a wallet view may load.
//
// Anyone can send a token to an address, and the token carries a logo address its creator
// picked. Loading it in a view of that address would tell the creator's host the visitor's IP at
// that moment. So in wallet views (Portfolio, per-address lists) an https logo loads only for a
// token the user acquired on their own:
// - an order of this wallet names it, because it placed an ask or bid with it, a fill of its bid
//   delivered it (claims included), or it sold it,
// - the user created it, by the creation records,
// - or the user clicked "Show logo" on it, which is remembered per token in this browser.
// Anything unknown counts as not acquired. Market views are not gated, except for a token only
// the wallet's holdings name (`logoScope`). ipfs:// logos and
// kascov.io's checked copies are not gated here either.
import { useEffect, useState } from 'preact/hooks';

export const LOGO_SHOWN_KEY = 'kob.logo.shown';
const MAX_SHOWN = 2000;
const HEX64 = /^[0-9a-f]{64}$/;

type Store = Pick<Storage, 'getItem' | 'setItem'>;
const browserStore = (): Store | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/**
 * The tokens named by the wallet's own orders (their token and a pair's quote) and by its creation
 * records, as lower-case ids.
 */
export function acquiredTokens(orders: readonly { token: string | null; pair?: { quote: string } | null }[], creations: readonly { covenantId: string }[]): Set<string> {
  const out = new Set<string>();
  const add = (v: string | null | undefined) => {
    const id = typeof v === 'string' ? v.toLowerCase() : '';
    if (HEX64.test(id)) out.add(id);
  };
  for (const o of orders) {
    add(o.token);
    add(o.pair?.quote);
  }
  for (const c of creations) add(c.covenantId);
  return out;
}

/** The ids the user chose to show, read from `store`. A missing, broken or huge value is empty. */
export function readShown(store: Store | null = browserStore()): Set<string> {
  try {
    const raw = store?.getItem(LOGO_SHOWN_KEY);
    if (!raw || raw.length > 200_000) return new Set();
    const v: unknown = JSON.parse(raw);
    return new Set(Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string' && HEX64.test(x)).slice(-MAX_SHOWN) : []);
  } catch {
    return new Set();
  }
}

/**
 * The gating rule. A wallet view withholds an https logo of a token that is neither acquired nor
 * shown by choice. Everything else is not gated here.
 */
export const withholdsLogo = (scope: 'wallet' | undefined, allowed: boolean, kind: 'https' | 'ipfs'): boolean => scope === 'wallet' && !allowed && kind === 'https';

/**
 * The scope a token's logo is gated under. A row only the wallet's holdings name (`source: 'held'`,
 * ui/market/held-list.ts) is a wallet view wherever it shows, the market list and the token's own
 * page included: no list but the holder's names that token, so its logo would tell its creator's
 * host who holds it.
 */
export const logoScope = (scope: 'wallet' | undefined, source: string | undefined): 'wallet' | undefined => scope ?? (source === 'held' ? 'wallet' : undefined);

class LogoGate {
  private acquired = new Set<string>();
  private owner: string | null = null;
  private shown: Set<string> | null = null;
  private listeners = new Set<() => void>();

  private shownSet(): Set<string> {
    return (this.shown ??= readShown());
  }
  private emit() {
    for (const l of this.listeners) l();
  }
  /**
   * Sets the acquired set of the wallet `owner`, by its key. A new wallet replaces the old set.
   * Unchanged content does not notify.
   */
  setAcquired(owner: string | null, ids: ReadonlySet<string>): void {
    const same = owner === this.owner && ids.size === this.acquired.size && [...ids].every((i) => this.acquired.has(i));
    if (same) return;
    this.owner = owner;
    this.acquired = new Set(ids);
    this.emit();
  }
  /** Whether a creator-hosted logo of `covenantId` may load in a wallet view. */
  allowed(covenantId: string): boolean {
    const id = covenantId.toLowerCase();
    return this.acquired.has(id) || this.shownSet().has(id);
  }
  /**
   * Remembers the user's "Show logo" click for this token. The in-page value is set first, so a
   * store that refuses writes still applies for this visit.
   */
  show(covenantId: string, store: Store | null = browserStore()): void {
    const id = covenantId.toLowerCase();
    const s = this.shownSet();
    s.add(id);
    this.emit();
    try {
      store?.setItem(LOGO_SHOWN_KEY, JSON.stringify([...s].slice(-MAX_SHOWN)));
    } catch {
      /* The choice holds for this visit only. */
    }
  }
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => void this.listeners.delete(fn);
  }
  /** For tests. */
  reset(): void {
    this.acquired = new Set();
    this.owner = null;
    this.shown = null;
    this.emit();
  }
}

/** The app's one logo gate. */
export const logoGate = new LogoGate();

/**
 * Whether a creator-hosted logo of this token may load in a wallet view. Re-renders when that
 * changes.
 */
export function useLogoAllowed(covenantId: string): boolean {
  const [, setN] = useState(0);
  useEffect(() => logoGate.subscribe(() => setN((n) => n + 1)), []);
  return logoGate.allowed(covenantId);
}
