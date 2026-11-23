// Which creator-hosted (https) logos may load.
//
// Anyone can send a token to an address, and the token carries a logo address its creator
// picked. Loading it while that address's wallet is connected would tell the creator's host the
// visitor's IP at that moment. So an https logo of a token the connected wallet holds loads only
// for a token the user acquired on their own, wherever it shows (the market list, the token and
// pair pages and the wallet views alike), whether or not any list names it:
// - an order this browser placed names it (its placement record), because it placed an ask or
//   bid with it, a fill of its bid delivered it (claims included), or it sold it. An order the
//   indexer merely lists under the wallet's key does not count: anyone can fund an order naming
//   another key as its maker,
// - a transaction the user signed here took it, placed an order of it or spent it (`actedTokens`),
// - the user created it, by the creation records,
// - or the user clicked "Show logo" on it, which is remembered per token in this browser.
// Anything unknown counts as not acquired, and a token that only arrived (a second unit, an airdrop
// beside a purchase of another token) acquires nothing. In the wallet views (Portfolio, per-address
// lists) every token is gated so. Elsewhere a token the wallet does not hold is not gated, and while
// the holdings are being read its logo waits (`holding`). ipfs:// logos and kascov.io's checked
// copies are not gated here either.
import { useEffect, useState } from 'preact/hooks';

export const LOGO_SHOWN_KEY = 'kob.logo.shown';
export const LOGO_ACTED_KEY = 'kob.logo.acted';
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
 * records, as lower-case ids. The caller passes only orders this browser placed.
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

/** What `actedTokens` reads of a decoded transaction (kob/decode.ts `SigningSummary`). */
export interface ActedSummary {
  take: { ref: { covenantId: string } } | null;
  orders: readonly { description: { tokenCovId: string; pair?: { quote: { covId: string } } | null } }[];
  net: { tokens: readonly { ref: { covenantId: string }; fromWallet: bigint }[] };
}

/**
 * The tokens a transaction the user signed acts on, as lower-case ids: the token it takes, the
 * tokens of the orders it places (a pair's quote too), and the tokens it spends from the wallet.
 * A token it only delivers to the wallet is not among them unless the user took it, so nobody but
 * the user can add one.
 */
export function actedTokens(s: ActedSummary): Set<string> {
  const ids = [
    s.take?.ref.covenantId,
    ...s.orders.flatMap((o) => [o.description.tokenCovId, o.description.pair?.quote.covId]),
    ...s.net.tokens.filter((t) => t.fromWallet > 0n).map((t) => t.ref.covenantId),
  ];
  return new Set(ids.map((v) => (typeof v === 'string' ? v.toLowerCase() : '')).filter((id) => HEX64.test(id)));
}

/**
 * The ids stored under `key` (the user's "Show logo" choices by default), read from `store`. A
 * missing, broken or huge value is empty.
 */
export function readShown(store: Store | null = browserStore(), key: string = LOGO_SHOWN_KEY): Set<string> {
  try {
    const raw = store?.getItem(key);
    if (!raw || raw.length > 200_000) return new Set();
    const v: unknown = JSON.parse(raw);
    return new Set(Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string' && HEX64.test(x)).slice(-MAX_SHOWN) : []);
  } catch {
    return new Set();
  }
}

/**
 * The gating rule. A gated view withholds an https logo of a token that is neither acquired nor
 * shown by choice. Everything else is not gated here.
 */
export const withholdsLogo = (scope: 'wallet' | undefined, allowed: boolean, kind: 'https' | 'ipfs'): boolean => scope === 'wallet' && !allowed && kind === 'https';

/**
 * What the gate knows of whether the connected wallet holds a token: `pending` while its holdings
 * are being read, `unknown` when nothing read them in this visit or no wallet is connected.
 */
export type Holding = 'held' | 'not-held' | 'pending' | 'unknown';

/**
 * The scope a token's logo is gated under. A token the connected wallet holds is gated wherever it
 * shows, the market list and its own page included, listed or not, since its logo would tell its
 * creator's host who holds it. A row only the wallet's holdings name (`source: 'held'`,
 * ui/market/held-list.ts) is held by definition.
 */
export const logoScope = (scope: 'wallet' | undefined, source: string | undefined, holding: Holding = 'unknown'): 'wallet' | undefined =>
  scope ?? (source === 'held' || holding === 'held' ? 'wallet' : undefined);

class LogoGate {
  private acquired = new Set<string>();
  private owner: string | null = null;
  private shown: Set<string> | null = null;
  private acted: Set<string> | null = null;
  private holder: string | null = null;
  private held = new Set<string>();
  /** The state of the full read of the holder's holdings, and of each one-token check. */
  private all: 'none' | 'pending' | 'read' = 'none';
  private checks = new Map<string, 'pending' | 'read'>();
  private listeners = new Set<() => void>();

  private shownSet(): Set<string> {
    return (this.shown ??= readShown());
  }
  private actedSet(): Set<string> {
    return (this.acted ??= readShown(browserStore(), LOGO_ACTED_KEY));
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
  /** Whether a creator-hosted logo of `covenantId` may load where it is gated. */
  allowed(covenantId: string): boolean {
    const id = covenantId.toLowerCase();
    return this.acquired.has(id) || this.shownSet().has(id) || this.actedSet().has(id);
  }
  private holderIs(owner: string): void {
    if (owner === this.holder) return;
    this.holder = owner;
    this.held = new Set();
    this.all = 'none';
    this.checks = new Map();
  }
  /**
   * A read of `owner`'s holdings started: all of them (`*`) or one token's. The logos it decides
   * wait for `noteHoldings`, whatever an older read said, since a unit may have arrived since.
   */
  expectHoldings(owner: string, scope: string): void {
    this.holderIs(owner);
    const id = scope.toLowerCase();
    if ((id === '*' ? this.all : this.checks.get(id)) === 'pending') return;
    if (id === '*') this.all = 'pending';
    else this.checks.set(id, 'pending');
    this.emit();
  }
  /**
   * The answer of a read of `owner`'s holdings (`*` or one token's): the tokens it holds. A token
   * once seen held stays so for the visit.
   */
  noteHoldings(owner: string, scope: string, held: Iterable<string>): void {
    this.holderIs(owner);
    for (const h of held) this.held.add(h.toLowerCase());
    const id = scope.toLowerCase();
    if (id === '*') this.all = 'read';
    else this.checks.set(id, 'read');
    this.emit();
  }
  /** Whether `owner` (the connected wallet's key) holds `covenantId`, as far as this visit read. */
  holding(owner: string | null, covenantId: string): Holding {
    if (!owner || owner !== this.holder) return 'unknown';
    const id = covenantId.toLowerCase();
    if (this.held.has(id)) return 'held';
    const one = this.checks.get(id);
    if (one === 'pending' || this.all === 'pending') return 'pending';
    return one === 'read' || this.all === 'read' ? 'not-held' : 'unknown';
  }
  /**
   * Remembers the user's "Show logo" click for this token. The in-page value is set first, so a
   * store that refuses writes still applies for this visit.
   */
  show(covenantId: string, store: Store | null = browserStore()): void {
    this.add(this.shownSet(), LOGO_SHOWN_KEY, [covenantId], store);
  }
  /** Remembers the tokens a transaction the user signed here acted on (`actedTokens`). */
  remember(ids: Iterable<string>, store: Store | null = browserStore()): void {
    this.add(this.actedSet(), LOGO_ACTED_KEY, ids, store);
  }
  private add(s: Set<string>, key: string, ids: Iterable<string>, store: Store | null): void {
    const list = [...ids].map((i) => i.toLowerCase());
    if (list.length === 0) return;
    for (const id of list) s.add(id);
    this.emit();
    try {
      store?.setItem(key, JSON.stringify([...s].slice(-MAX_SHOWN)));
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
    this.acted = null;
    this.holder = null;
    this.held = new Set();
    this.all = 'none';
    this.checks = new Map();
    this.emit();
  }
}

/** The app's one logo gate. */
export const logoGate = new LogoGate();

/**
 * Whether a creator-hosted logo of this token may load where it is gated. Re-renders when that
 * changes.
 */
export function useLogoAllowed(covenantId: string): boolean {
  const [, setN] = useState(0);
  useEffect(() => logoGate.subscribe(() => setN((n) => n + 1)), []);
  return logoGate.allowed(covenantId);
}

/** `logoGate.holding` for the connected wallet's key. Re-renders when it changes. */
export function useLogoHolding(owner: string | null, covenantId: string): Holding {
  const [, setN] = useState(0);
  useEffect(() => logoGate.subscribe(() => setN((n) => n + 1)), []);
  return logoGate.holding(owner, covenantId);
}
