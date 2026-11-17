// Which creator-hosted (https) logos a wallet view may load (review M5). A token anyone can send to an address carries a logo address its creator picked; loading
// it in a view of that address would tell the creator's host the visitor's IP at that moment. So in wallet views (Portfolio, per-address lists) an https logo loads
// only for a token the user acquired on their own:
//   - an order of this wallet names it (it placed an ask or bid with it, a fill of its bid delivered it, claims included, or it sold it);
//   - the user created it (the creation records);
//   - or the user clicked "Show logo" on it (remembered per token in this browser).
// Anything unknown counts as not acquired. Market views are not gated. ipfs:// logos and kascov.io's checked copies are not gated here either.
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

/** The tokens named by the wallet's own orders (their token and a pair's quote) and by its creation records; lower-case ids. */
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

/** The ids the user chose to show, read from `store`; a missing, broken or oversized value is empty. */
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

/** The gating rule: a wallet view withholds an https logo of a token that is neither acquired nor shown by choice; everything else is not gated here. */
export const withholdsLogo = (scope: 'wallet' | undefined, allowed: boolean, kind: 'https' | 'ipfs'): boolean => scope === 'wallet' && !allowed && kind === 'https';

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
  /** The acquired set of the wallet `owner` (its key); a new wallet replaces the old set. Unchanged content does not notify. */
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
  /** The user's "Show logo" click: remembered for this token (the in-page value is set first; a store that refuses writes still applies for this visit). */
  show(covenantId: string, store: Store | null = browserStore()): void {
    const id = covenantId.toLowerCase();
    const s = this.shownSet();
    s.add(id);
    this.emit();
    try {
      store?.setItem(LOGO_SHOWN_KEY, JSON.stringify([...s].slice(-MAX_SHOWN)));
    } catch {
      /* the choice holds for this visit only */
    }
  }
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => void this.listeners.delete(fn);
  }
  /** for tests */
  reset(): void {
    this.acquired = new Set();
    this.owner = null;
    this.shown = null;
    this.emit();
  }
}

export const logoGate = new LogoGate();

/** Whether a creator-hosted logo of this token may load in a wallet view; re-renders when that changes. */
export function useLogoAllowed(covenantId: string): boolean {
  const [, setN] = useState(0);
  useEffect(() => logoGate.subscribe(() => setN((n) => n + 1)), []);
  return logoGate.allowed(covenantId);
}
