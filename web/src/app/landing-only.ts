// The build's landing-only mode (build-time flag `KOB_LANDING_ONLY`, vite.config.ts `define: __KOB_LANDING_ONLY__`; docs/ops/web-hosting.md, "The mainnet front page").
//
//   false (default)  the app: landing page, trading UI, wallet, node and indexer connections (every testnet-10 deployment, the soak host, `vite preview`).
//   true             the production front page of a network that has not launched: the landing page only, "in preparation", with a link to the testnet app.
//                    The build has its own entry (src/main-landing.tsx, swapped in for src/main.tsx by vite.config.ts): the router, the services, the wallet
//                    layer, the wasm engine and the trading views are not part of the bundle, so nothing that can lock funds is there to reach. No node or
//                    indexer connection, no wallet, no service worker, no private-key signer page; the page's CSP has `connect-src 'none'`.
//
// Why a build flag and not config.json: the deployed config is data a host or a link could change; a flag the bundle was built with cannot be switched by
// anyone but a new build of this repository (deploy-pages.yml sets it for the mainnet site in the workflow file itself, so launching mainnet is a reviewed change).

/** The KOB_LANDING_ONLY value of a build: unset / empty / `0` / `false` = the app, `1` / `true` = landing only; anything else fails the build (a typo must not build the app). */
export function parseLandingOnly(raw: string | undefined): boolean {
  const v = (raw ?? '').trim().toLowerCase();
  if (v === '' || v === '0' || v === 'false') return false;
  if (v === '1' || v === 'true') return true;
  throw new Error(`KOB_LANDING_ONLY must be empty, 0, false, 1 or true, got "${(raw ?? '').trim()}"`);
}

/** Whether this bundle was built landing-only. */
export const LANDING_ONLY: boolean = typeof __KOB_LANDING_ONLY__ === 'boolean' && __KOB_LANDING_ONLY__;

/** Where "Try it on testnet" leads: the testnet-10 app (docs/ops/web-hosting.md). */
export const TESTNET_APP_URL = 'https://kob-tn10.kaspa.ag/#/market';

/**
 * The landing-only page has one route: whatever hash the address carries (`#/market`, `#/token/...`, `#/orders`, a typo), it shows the landing page. The
 * address is cleaned to the page itself, so a copied link does not suggest an app page that is not there. Returns the URL to replace the address with, or
 * null when it is already the page (no hash, `#` or `#/`).
 */
export function cleanLandingUrl(loc: { pathname: string; search: string; hash: string }): string | null {
  if (loc.hash === '' || loc.hash === '#' || loc.hash === '#/') return null;
  return `${loc.pathname}${loc.search}`;
}

/** Keeps the address on the page (cleanLandingUrl) now and after every hash change; returns the stop function. */
export function keepLandingUrl(w: Pick<Window, 'location' | 'history' | 'addEventListener' | 'removeEventListener'>): () => void {
  const fix = () => {
    const clean = cleanLandingUrl(w.location);
    if (clean !== null) w.history.replaceState(w.history.state, '', clean);
  };
  fix();
  w.addEventListener('hashchange', fix);
  return () => w.removeEventListener('hashchange', fix);
}
