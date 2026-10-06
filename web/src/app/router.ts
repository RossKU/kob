// Hash router: `#/market`, `#/market/<covenantId>` (the token's KAS market), `#/market/<baseCovenantId>/<quoteCovenantId>` (a token/token pair; the old `#/pair/<base>/<quote>` is an alias), `#/usd/kas` + `#/usd/<covenantId>` (legacy: redirected to the tradable USD pages), `#/orders`, `#/issue`,
// `#/settings`. An empty hash is the landing screen (`home`: config `home`, by default a chart); an unknown one is the market list.
// The parsing / formatting is pure (tested in router.test.ts); only `useRoute` / `navigate` touch `window`.
import { useEffect, useState } from 'preact/hooks';

export type Route =
  /** the landing screen (empty hash): resolved by the app from config `home` (a chart by default, or the market list) */
  | { name: 'home'; covenantId?: undefined }
  | { name: 'market'; covenantId?: undefined; invalidToken?: string }
  /**
   * LEGACY `#/usd/kas` / `#/usd/<covenant id>` links: not a page of its own any more, the app redirects them to the tradable page (`usdMarketHash`:
   * the USD token's market for `kas`, the pair page `<token>/<USD token>` for a token). `asset` = `kas` or a token's covenant id
   */
  | { name: 'usd'; asset: string; covenantId?: undefined }
  /**
   * `ticket`: the order ticket opens on this preset (`#/market/<id>?ticket=close`: sell everything at market, used by "close position" of a buy-first
   * position; `?ticket=cover&amount=N`: buy N base units back at market, the close of a sell-first position)
   */
  | { name: 'token'; covenantId: string; ticket?: TicketPreset; amount?: string }
  /** token/token pair BASE/QUOTE (pair orders of every type, prices in QUOTE per BASE): `#/market/<base>/<quote>` (alias `#/pair/<base>/<quote>`) */
  | { name: 'pair'; base: string; quote: string; covenantId?: undefined }
  | { name: 'orders' | 'issue' | 'settings'; covenantId?: undefined };

export type RouteName = Route['name'];

/** Order-ticket presets a link may ask for. */
export const TICKET_PRESETS = ['close', 'cover'] as const;
export type TicketPreset = (typeof TICKET_PRESETS)[number];

const HEX64 = /^[0-9a-f]{64}$/i;
const PAGES = ['orders', 'issue', 'settings'] as const;

/** Parses a `location.hash` value (with or without the leading `#`). Never throws. */
export function parseHash(hash: string): Route {
  let h = (hash ?? '').trim();
  if (h.startsWith('#')) h = h.slice(1);
  const cut = h.search(/[?]/);
  const query = cut >= 0 ? new URLSearchParams(h.slice(cut + 1)) : null;
  if (cut >= 0) h = h.slice(0, cut);
  const preset = query?.get('ticket') ?? null;
  const ticket = (TICKET_PRESETS as readonly string[]).includes(preset ?? '') ? (preset as TicketPreset) : undefined;
  // an amount in base units (1..10^18) for the cover preset; anything else is dropped
  const amountRaw = query?.get('amount') ?? '';
  const amount = ticket === 'cover' && /^[1-9][0-9]{0,17}$/.test(amountRaw) ? amountRaw : undefined;
  const parts = h.split('/').filter((p) => p !== '');
  const [first, second, third] = parts;
  if (!first) return { name: 'home' };
  const page = first.toLowerCase();
  if (page === 'usd') {
    const a = (second ?? '').toLowerCase();
    if (a === 'kas' || HEX64.test(a)) return { name: 'usd', asset: a };
    return { name: 'market', invalidToken: (second ?? '').slice(0, 80) };
  }
  const dec = (v: string | undefined): string => {
    try {
      return decodeURIComponent(v ?? '');
    } catch {
      return v ?? '';
    }
  };
  // a third segment that is neither `kas` nor an id is ignored (as before): `#/market/<id>/anything` is the token market
  const quoteSeg = page === 'market' && third !== undefined && (third.toLowerCase() === 'kas' || HEX64.test(dec(third)));
  if (page === 'pair' || quoteSeg) {
    const b = dec(second);
    const q = dec(third);
    // `#/market/<token>/kas` is the token's own KAS market
    if (page === 'market' && HEX64.test(b) && q.toLowerCase() === 'kas') return { name: 'token', covenantId: b.toLowerCase(), ...(ticket ? { ticket } : {}), ...(amount ? { amount } : {}) };
    if (HEX64.test(b) && HEX64.test(q) && b.toLowerCase() !== q.toLowerCase()) return { name: 'pair', base: b.toLowerCase(), quote: q.toLowerCase() };
    return { name: 'market', invalidToken: `${b}/${q}`.slice(0, 80) };
  }
  if (page === 'market') {
    if (second === undefined) return { name: 'market' };
    let id = second;
    try {
      id = decodeURIComponent(second);
    } catch {
      /* keep the raw text */
    }
    return HEX64.test(id) ? { name: 'token', covenantId: id.toLowerCase(), ...(ticket ? { ticket } : {}), ...(amount ? { amount } : {}) } : { name: 'market', invalidToken: id.slice(0, 80) };
  }
  if ((PAGES as readonly string[]).includes(page)) return { name: page as (typeof PAGES)[number] };
  return { name: 'market' };
}

/** Route -> hash (with the leading `#`). `parseHash(routeToHash(r))` gives `r` back for every valid route. */
export function routeToHash(route: Route): string {
  switch (route.name) {
    case 'token':
      return `#/market/${route.covenantId.toLowerCase()}${route.ticket ? `?ticket=${route.ticket}${route.ticket === 'cover' && route.amount ? `&amount=${route.amount}` : ''}` : ''}`;
    case 'market':
      return '#/market';
    case 'pair':
      return `#/market/${route.base.toLowerCase()}/${route.quote.toLowerCase()}`;
    case 'usd':
      return `#/usd/${route.asset.toLowerCase()}`;
    case 'home':
      return '#/';
    default:
      return `#/${route.name}`;
  }
}

/** Which top-level nav entry a route belongs to. */
export const navOf = (route: Route): 'market' | 'orders' | 'issue' | 'settings' =>
  route.name === 'token' || route.name === 'pair' || route.name === 'usd' || route.name === 'home' ? 'market' : route.name;

export const tokenRoute = (covenantId: string, ticket?: TicketPreset, amount?: number | bigint): Route => ({
  name: 'token', covenantId: covenantId.toLowerCase(), ...(ticket ? { ticket } : {}), ...(ticket === 'cover' && amount !== undefined && BigInt(amount) > 0n ? { amount: BigInt(amount).toString() } : {}),
});
export const pairRoute = (base: string, quote: string): Route => ({ name: 'pair', base: base.toLowerCase(), quote: quote.toLowerCase() });
/**
 * Where an old USD link goes (there is no USD page, all settlement is through the KAS books, so every view is tradable): `kas` (and the USD token itself)
 * to the USD token's own market page, a token to the pair page `<token>/<USD token>` (implied book and chart, cross ticket through the KAS route).
 * Tickers are shown as they are.
 */
export const usdMarketRoute = (asset: string, usdToken: string): Route =>
  asset.toLowerCase() === 'kas' || asset.toLowerCase() === usdToken.toLowerCase() ? tokenRoute(usdToken) : pairRoute(asset, usdToken);
export const usdMarketHash = (asset: string, usdToken: string): string => routeToHash(usdMarketRoute(asset, usdToken));

const currentHash = (): string => (typeof window === 'undefined' ? '' : window.location.hash);

export function navigate(route: Route): void {
  if (typeof window === 'undefined') return;
  const h = routeToHash(route);
  if (window.location.hash !== h) window.location.hash = h;
}

/** Current route; re-renders on `hashchange`. */
export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseHash(currentHash()));
  useEffect(() => {
    const on = () => setRoute(parseHash(currentHash()));
    window.addEventListener('hashchange', on);
    on();
    return () => window.removeEventListener('hashchange', on);
  }, []);
  return route;
}
