// The "Trade" tab of the header leads back to the market the viewer opened last. That is a token's
// KAS market (`#/market/<token>`) or a token/token pair (`#/market/<base>/<quote>`).
//
// The choice is kept per network in localStorage. Every access is guarded, so a private window,
// blocked site data or a full disk store nothing and never break the page.
//
// The tab is always shown, because the tab bar must not shift between pages. With nothing stored
// it leads to the first market the registry lists (`tradeTarget`), and with none of those to the
// market list. A stored value that is no market route is dropped when it is read. A market that
// does not resolve, such as the "not found" of the token page and the pair page, is forgotten by
// `ForgetMarket`.
import { useEffect, useState } from 'preact/hooks';
import type { TokenRegistry } from '../kob/registry';
import { parseHash, pairRoute, routeToHash, tokenRoute, type Route } from './router';

/** The localStorage key of the last market of `network`. */
export const lastMarketKey = (network: string): string => `kob.lastMarket.${network}`;

/** The market a route is: a token page without its ticket preset, or a pair page. Else null. */
export function marketRouteOf(route: Route): Route | null {
  if (route.name === 'token') return tokenRoute(route.covenantId);
  if (route.name === 'pair') return pairRoute(route.base, route.quote);
  return null;
}

function storage(): Storage | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

const listeners = new Set<() => void>();

/**
 * Remembers `route` as the last market of `network` when it is one. A quiet write: no listener is
 * told, because the asking header is rendering.
 */
export function rememberMarket(network: string, route: Route): void {
  const m = marketRouteOf(route);
  if (!m) return;
  try {
    const key = lastMarketKey(network);
    const hash = routeToHash(m);
    const s = storage();
    if (s && s.getItem(key) !== hash) s.setItem(key, hash);
  } catch {
    /* No storage, so no Trade tab. */
  }
}

/**
 * The hash of the last market of `network`. Null when nothing is stored or the stored value is no
 * market route, and then it is removed.
 */
export function lastMarketHash(network: string): string | null {
  try {
    const s = storage();
    const v = s?.getItem(lastMarketKey(network)) ?? null;
    if (v === null) return null;
    const m = marketRouteOf(parseHash(v));
    if (m && routeToHash(m) === v) return v;
    s?.removeItem(lastMarketKey(network));
  } catch {
    /* No storage. */
  }
  return null;
}

/** Forgets the last market of `network`, because it no longer resolves, and tells the header. */
export function clearLastMarket(network: string): void {
  try {
    storage()?.removeItem(lastMarketKey(network));
  } catch {
    /* No storage. */
  }
  for (const l of [...listeners]) l();
}

/**
 * The Trade tab's target on `network`. A route that is a market is remembered first, once per
 * arrival, and then the stored hash is read. It re-reads when a market is forgotten.
 */
export function useLastMarket(network: string, route: Route): string | null {
  const [, tick] = useState(0);
  useEffect(() => {
    const l = () => tick((n) => n + 1);
    listeners.add(l);
    return () => void listeners.delete(l);
  }, []);
  const m = marketRouteOf(route);
  const key = m ? `${network} ${routeToHash(m)}` : '';
  // Remember on arrival at a market, not on every render. A market forgotten while its page is
  // open must stay forgotten.
  const [seen] = useState<{ key: string }>({ key: '' });
  if (key && seen.key !== key) {
    seen.key = key;
    rememberMarket(network, route);
  }
  return lastMarketHash(network);
}

/**
 * Where the Trade tab leads: the last market opened (`last`), else the first tradable token the
 * registry lists, else the market list. The registry is loaded at boot, so this makes no request.
 * Never null, so the tab is always there.
 */
export function tradeTarget(last: string | null, registry: Pick<TokenRegistry, 'tokens'>): string {
  if (last) return last;
  const first = registry.tokens.find((tk) => tk.tradable);
  return first ? routeToHash(tokenRoute(first.covenantId)) : '#/market';
}
