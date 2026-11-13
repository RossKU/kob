// The browser tab's title per page: the page's name (`Market`, `Portfolio`, ...) or, on a market, the token's name or the pair's, then the app's name.
// Every view sets its own with `useDocumentTitle` while it is mounted (a view that knows more than the route, such as a token's ticker, says it), so the
// shell never overwrites what a view set: effects of children run before the parent's.
import type { ComponentChildren } from 'preact';
import { useEffect } from 'preact/hooks';
import { t } from '../i18n';
import { shortId } from '../ui/kit/format';

// The network in force names itself in the tab: a testnet page is never mistaken for the real one. It is read at runtime (one bundle serves both networks),
// so it applies on top of whatever the build wrote (head-meta.mjs may have prefixed a testnet build's static title: that prefix is replaced, not doubled).
const TESTNET_PREFIX = 'Testnet · ';
let titlePrefix = '';
const unprefixed = (s: string): string => (s.startsWith(TESTNET_PREFIX) ? s.slice(TESTNET_PREFIX.length) : s);

/** The title `title` as it shows in the tab on the network in force. */
export const tabTitle = (title: string): string => titlePrefix + unprefixed(title);

/** Called once at boot with the network in force: prefixes the tab title, og:title and twitter:title of a testnet-10 page (and clears a build-time prefix on mainnet). og:url and the other static tags stay as built. */
export function applyNetworkToHead(network: string, doc: Document = document): void {
  titlePrefix = network === 'mainnet' ? '' : network === 'testnet-10' ? TESTNET_PREFIX : '';
  doc.title = tabTitle(doc.title);
  for (const sel of ['meta[property="og:title"]', 'meta[name="twitter:title"]']) {
    const m = doc.head.querySelector<HTMLMetaElement>(sel);
    const c = m?.getAttribute('content');
    if (m && c !== null && c !== undefined) m.setAttribute('content', tabTitle(c));
  }
}

/** `<page> · KOB` (founder 2026-11-11: `EXKCC/KAS · KOB`). */
export const pageTitle = (page: string): string => `${page} · ${t('common.appName')}`;

/**
 * The title of a market's page without a 64-hex covenant id (review M-05): `EXKCC/KAS · KOB`, `EXKCC · KOB`. `name` is the token's ticker or name; a token that has neither shows its id
 * shortened (`e5e5…e5e5`), never in full. `quote` is the pair's other side (a pair page); omitted on a token's own page.
 */
export function marketPageTitle(base: { name?: string | null; id: string }, quote?: { name?: string | null; id: string } | null): string {
  const label = (x: { name?: string | null; id: string }): string => x.name?.trim() || shortId(x.id);
  return pageTitle(quote ? `${label(base)}/${label(quote)}` : label(base));
}

/** The landing title (the app's name and what it is): the page title of the legacy redirect, which shows no view. */
export const appTitle = (): string => `${t('common.appName')} - ${t('common.tagline')}`;

/** A view of a page that has no title logic of its own: sets the title of the page `page` names (a key of `shell.nav.*`) and shows `children`. */
export function Titled(props: { page: 'market' | 'activity' | 'portfolio' | 'create' | 'settings' | 'disclaimer' | 'support'; children?: ComponentChildren }) {
  useDocumentTitle(pageTitle(t(`shell.nav.${props.page}`)));
  return <>{props.children}</>;
}

/** Sets `document.title` while the calling view is mounted (and whenever `title` changes); null leaves it alone. */
export function useDocumentTitle(title: string | null): void {
  useEffect(() => {
    if (title !== null) document.title = tabTitle(title);
  }, [title]);
}
