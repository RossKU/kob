// The browser tab's title per page: the page's name (`Market`, `My orders`, ...) or, on a market, the token's name or the pair's, then the app's name.
// Every view sets its own with `useDocumentTitle` while it is mounted (a view that knows more than the route, such as a token's ticker, says it), so the
// shell never overwrites what a view set: effects of children run before the parent's.
import type { ComponentChildren } from 'preact';
import { useEffect } from 'preact/hooks';
import { t } from '../i18n';

/** `<page> - KOB`. */
export const pageTitle = (page: string): string => `${page} - ${t('common.appName')}`;

/** The landing title (the app's name and what it is): the page title of the legacy redirect, which shows no view. */
export const appTitle = (): string => `${t('common.appName')} - ${t('common.tagline')}`;

/** A view of a page that has no title logic of its own: sets the title of the page `page` names (a key of `shell.nav.*`) and shows `children`. */
export function Titled(props: { page: 'market' | 'orders' | 'create' | 'settings'; children?: ComponentChildren }) {
  useDocumentTitle(pageTitle(t(`shell.nav.${props.page}`)));
  return <>{props.children}</>;
}

/** Sets `document.title` while the calling view is mounted (and whenever `title` changes); null leaves it alone. */
export function useDocumentTitle(title: string | null): void {
  useEffect(() => {
    if (title !== null) document.title = title;
  }, [title]);
}
