// The "Trade" tab of the header leads back to the market the viewer opened last: a token's KAS market (`#/market/<token>`) or a token/token pair
// (`#/market/<base>/<quote>`), whichever it was. Kept per network in localStorage (every access guarded: a private window, blocked site data and a full
// disk leave the tab out, never break the page). Nothing stored: no tab. A stored value that is no market route is dropped when it is read; a market that
// does not resolve (the token page's and the pair page's "not found") is forgotten by `ForgetMarket`.
import { useEffect, useState } from 'preact/hooks';
import { parseHash, pairRoute, routeToHash, tokenRoute, type Route } from './router';

export const lastMarketKey = (network: string): string => `kob.lastMarket.${network}`;

/** The market a route is (a token page without its ticket preset, or a pair page); null for every other route. */
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

/** Remembers `route` as the last market of `network` when it is one (a quiet write: no listener is told, the header that asks is rendering). */
export function rememberMarket(network: string, route: Route): void {
  const m = marketRouteOf(route);
  if (!m) return;
  try {
    const key = lastMarketKey(network);
    const hash = routeToHash(m);
    const s = storage();
    if (s && s.getItem(key) !== hash) s.setItem(key, hash);
  } catch {
    /* no storage: no Trade tab */
  }
}

/** The hash of the last market of `network`, or null: nothing stored, or what is stored is no market route (it is removed). */
export function lastMarketHash(network: string): string | null {
  try {
    const s = storage();
    const v = s?.getItem(lastMarketKey(network)) ?? null;
    if (v === null) return null;
    const m = marketRouteOf(parseHash(v));
    if (m && routeToHash(m) === v) return v;
    s?.removeItem(lastMarketKey(network));
  } catch {
    /* no storage */
  }
  return null;
}

/** Forgets the last market of `network` (it no longer resolves) and tells the header. */
export function clearLastMarket(network: string): void {
  try {
    storage()?.removeItem(lastMarketKey(network));
  } catch {
    /* no storage */
  }
  for (const l of [...listeners]) l();
}

/** The Trade tab's target on `network`: the route is remembered first when it is a market (once per arrival), then the stored hash is read. Re-reads when a market is forgotten. */
export function useLastMarket(network: string, route: Route): string | null {
  const [, tick] = useState(0);
  useEffect(() => {
    const l = () => tick((n) => n + 1);
    listeners.add(l);
    return () => void listeners.delete(l);
  }, []);
  const m = marketRouteOf(route);
  const key = m ? `${network} ${routeToHash(m)}` : '';
  // remember on arrival at a market, not on every render: a market forgotten while its page is open must stay forgotten
  const [seen] = useState<{ key: string }>({ key: '' });
  if (key && seen.key !== key) {
    seen.key = key;
    rememberMarket(network, route);
  }
  return lastMarketHash(network);
}
