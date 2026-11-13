// Hash router. The routes are:
// - `#/market` is the market list.
// - `#/market/<covenantId>` is a token's KAS market.
// - `#/market/<baseCovenantId>/<quoteCovenantId>` is a token/token pair.
// - `#/activity` is the activity log. `?market=<id>` or `?market=<base>/<quote>` narrows it to one
//   market.
// - `#/portfolio` is the Portfolio page. `#/portfolio/<tab>` and `#/portfolio/recover` open a tab.
// - `#/settings`, `#/disclaimer` and `#/support`. Support shows the market list while there is no
//   donation address (ui/support/support-model.ts).
// An empty hash is the landing screen (`home`). Config `home` chooses it: by default the landing
// page (src/ui/landing), or a chart or the list. An unknown hash is the market list.
//
// Parsing and formatting are pure and tested in router.test.ts. Only `useRoute` and `navigate`
// touch `window`.
import { useEffect, useState } from 'preact/hooks';

/** A parsed location hash. */
export type Route =
  /** The landing screen (empty hash). The app resolves it from config `home`. */
  | { name: 'home'; covenantId?: undefined }
  | { name: 'market'; covenantId?: undefined; invalidToken?: string }
  /**
   * `ticket` opens the order ticket on a preset.
   * - `?ticket=sell&amount=N` sells N base units at market, the close of a buy-first position.
   * - `?ticket=cover&amount=N` buys N base units back at market, the close of a sell-first one.
   * - `?ticket=close` is the "close (sell all)" type.
   */
  | { name: 'token'; covenantId: string; ticket?: TicketPreset; amount?: string }
  /** Token/token pair BASE/QUOTE (`#/market/<base>/<quote>`), with prices in QUOTE per BASE. */
  | { name: 'pair'; base: string; quote: string; covenantId?: undefined }
  /**
   * `#/activity` is the activity log of every market. `?market=<covenant id>` narrows it to one
   * token's KAS book and `?market=<base>/<quote>` to one pair. Such a link can be shared.
   */
  | { name: 'activity'; market?: ActivityMarket; covenantId?: undefined }
  /**
   * `#/portfolio` is the Portfolio page. `#/portfolio/<tab>` opens one of its tabs (balances, open
   * orders, order history, fills), and the first tab, Balances, is the default.
   * `#/portfolio/recover` opens the page with its folded section of strays, automatic refunds and
   * recovery.
   */
  | { name: 'portfolio'; tab?: PortfolioTab; recover?: true; covenantId?: undefined }
  | { name: 'create' | 'settings' | 'disclaimer' | 'support'; covenantId?: undefined };

/** The Portfolio page's tabs, in order (ui/orders/OrdersView.tsx). */
export const PORTFOLIO_TABS = ['balances', 'open', 'history', 'fills'] as const;
/** One tab of the Portfolio page. */
export type PortfolioTab = (typeof PORTFOLIO_TABS)[number];

/** The market an activity link names: one token's KAS book or one token/token pair. */
export type ActivityMarket = { kind: 'token'; token: string } | { kind: 'pair'; base: string; quote: string };

/** The name of a route. */
export type RouteName = Route['name'];

/** Order-ticket presets a link may ask for. */
export const TICKET_PRESETS = ['close', 'cover', 'sell'] as const;
/** Presets that carry an amount (`&amount=N`, in base units). */
const AMOUNT_PRESETS: readonly TicketPreset[] = ['cover', 'sell'];
const takesAmount = (p: TicketPreset | undefined): boolean => p !== undefined && AMOUNT_PRESETS.includes(p);
/** One order-ticket preset. */
export type TicketPreset = (typeof TICKET_PRESETS)[number];

const HEX64 = /^[0-9a-f]{64}$/i;
const PAGES = ['create', 'settings', 'disclaimer', 'support'] as const;

/** Parses a `location.hash` value (with or without the leading `#`). Never throws. */
export function parseHash(hash: string): Route {
  let h = (hash ?? '').trim();
  if (h.startsWith('#')) h = h.slice(1);
  const cut = h.search(/[?]/);
  const query = cut >= 0 ? new URLSearchParams(h.slice(cut + 1)) : null;
  if (cut >= 0) h = h.slice(0, cut);
  const preset = query?.get('ticket') ?? null;
  const ticket = (TICKET_PRESETS as readonly string[]).includes(preset ?? '') ? (preset as TicketPreset) : undefined;
  // An amount in base units (1..10^18) for the cover and sell presets. Anything else is dropped.
  const amountRaw = query?.get('amount') ?? '';
  const amount = takesAmount(ticket) && /^[1-9][0-9]{0,17}$/.test(amountRaw) ? amountRaw : undefined;
  const parts = h.split('/').filter((p) => p !== '');
  const [first, second, third] = parts;
  if (!first) return { name: 'home' };
  const page = first.toLowerCase();
  const dec = (v: string | undefined): string => {
    try {
      return decodeURIComponent(v ?? '');
    } catch {
      return v ?? '';
    }
  };
  // A third segment that is neither `kas` nor an id is ignored, so `#/market/<id>/anything` is the
  // token market.
  const quoteSeg = page === 'market' && third !== undefined && (third.toLowerCase() === 'kas' || HEX64.test(dec(third)));
  if (quoteSeg) {
    const b = dec(second);
    const q = dec(third);
    // `#/market/<token>/kas` is the token's own KAS market.
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
  if (page === 'activity') {
    const market = activityMarketOf(query?.get('market') ?? '');
    return market ? { name: 'activity', market } : { name: 'activity' };
  }
  if (page === 'portfolio') {
    const tab = (second ?? '').toLowerCase();
    if (tab === 'recover') return { name: 'portfolio', recover: true };
    return (PORTFOLIO_TABS as readonly string[]).includes(tab) ? { name: 'portfolio', tab: tab as PortfolioTab } : { name: 'portfolio' };
  }
  if ((PAGES as readonly string[]).includes(page)) return { name: page as (typeof PAGES)[number] };
  return { name: 'market' };
}

/** Route to hash, with the leading `#`. `parseHash(routeToHash(r))` returns `r` if valid. */
export function routeToHash(route: Route): string {
  switch (route.name) {
    case 'token':
      return `#/market/${route.covenantId.toLowerCase()}${route.ticket ? `?ticket=${route.ticket}${takesAmount(route.ticket) && route.amount ? `&amount=${route.amount}` : ''}` : ''}`;
    case 'market':
      return '#/market';
    case 'pair':
      return `#/market/${route.base.toLowerCase()}/${route.quote.toLowerCase()}`;
    case 'home':
      return '#/';
    case 'activity':
      return route.market ? `#/activity?market=${activityMarketParam(route.market)}` : '#/activity';
    case 'portfolio':
      return route.recover ? '#/portfolio/recover' : route.tab ? `#/portfolio/${route.tab}` : '#/portfolio';
    default:
      return `#/${route.name}`;
  }
}

/**
 * The market of an activity link's `?market=`: a covenant id (a token's KAS book) or
 * `<base>/<quote>` (a pair). Anything else is null.
 */
export function activityMarketOf(m: string): ActivityMarket | null {
  const [a, b, extra] = m.split('/');
  if (extra !== undefined || !a || !HEX64.test(a)) return null;
  if (b === undefined) return { kind: 'token', token: a.toLowerCase() };
  return HEX64.test(b) && a.toLowerCase() !== b.toLowerCase() ? { kind: 'pair', base: a.toLowerCase(), quote: b.toLowerCase() } : null;
}
/** The `?market=` value of an activity market. */
export const activityMarketParam = (m: ActivityMarket): string => (m.kind === 'token' ? m.token.toLowerCase() : `${m.base.toLowerCase()}/${m.quote.toLowerCase()}`);
/** The route of the activity page, narrowed to `market` when given. */
export const activityRoute = (market?: ActivityMarket | null): Route => (market ? { name: 'activity', market } : { name: 'activity' });

/** The top-level nav entry of a route. A token's or pair's market is Trade, the list is Market. */
export const navOf = (route: Route): 'market' | 'trade' | 'activity' | 'portfolio' | 'create' | 'settings' | 'disclaimer' | 'support' =>
  route.name === 'token' || route.name === 'pair' ? 'trade' : route.name === 'home' ? 'market' : route.name;

/** The route of a token's KAS market, with an optional ticket preset and amount. */
export const tokenRoute = (covenantId: string, ticket?: TicketPreset, amount?: number | bigint): Route => ({
  name: 'token', covenantId: covenantId.toLowerCase(), ...(ticket ? { ticket } : {}), ...(takesAmount(ticket) && amount !== undefined && BigInt(amount) > 0n ? { amount: BigInt(amount).toString() } : {}),
});
/** The route of a token/token pair. */
export const pairRoute = (base: string, quote: string): Route => ({ name: 'pair', base: base.toLowerCase(), quote: quote.toLowerCase() });

const currentHash = (): string => (typeof window === 'undefined' ? '' : window.location.hash);

/** Sets the location hash to `route`. */
export function navigate(route: Route): void {
  if (typeof window === 'undefined') return;
  const h = routeToHash(route);
  if (window.location.hash !== h) window.location.hash = h;
}

/**
 * As `navigate`, replacing the current history entry. A page that corrects its own address uses
 * it, so Back does not return to it.
 */
export function replaceRoute(route: Route): void {
  if (typeof window === 'undefined') return;
  const h = routeToHash(route);
  if (window.location.hash !== h) window.location.replace(h);
}

/** The current route. Re-renders on `hashchange`. */
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
