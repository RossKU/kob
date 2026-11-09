// The footer's facts (pure, tested in frame-model.test.ts): which links it shows on which page, and whether it carries the chart library's attribution.
import { APP_HASH, EXECUTOR_URL, publicApiUrl, REPO_URL } from '../landing/landing-model';

/** Which page the frame is on: the landing page (no charts, no app links) or the app shell. */
export type FrameContext = 'landing' | 'app';

export interface FooterLink {
  /** Dictionary key of its text. */
  key: string;
  href: string;
  /** Opens in a new tab (a site that is not the project's). */
  external?: boolean;
  testid?: string;
  /** Its tooltip. */
  title?: string;
  /** `site`: the links of every page; `app`: the app's own (after a divider). */
  group: 'site' | 'app';
}

/**
 * The footer's one row of links: App, GitHub, Run an executor, when the deployment has an indexer its Public API, when there is a donation address Support (`support`),
 * and the Disclaimer; the app adds Settings and data, Recover orders and kaspa.org.
 */
export function footerLinks(context: FrameContext, indexerUrl: string | null | undefined, support = false): FooterLink[] {
  const api = publicApiUrl(indexerUrl);
  const site: FooterLink[] = [
    { key: 'landing.footer.app', href: APP_HASH, group: 'site' },
    { key: 'landing.github', href: REPO_URL, group: 'site' },
    { key: 'landing.footer.executor', href: EXECUTOR_URL, group: 'site' },
    ...(api ? [{ key: 'landing.footer.api', href: api, testid: context === 'app' ? 'footer-api' : 'landing-api', group: 'site' } as const] : []),
    ...(support ? [{ key: 'landing.footer.support', href: '#/support', testid: 'footer-support', group: 'site' } as const] : []),
    { key: 'landing.footer.disclaimer', href: '#/disclaimer', testid: 'footer-disclaimer', group: 'site' },
  ];
  if (context !== 'app') return site;
  return [
    ...site,
    { key: 'shell.footer.settings', href: '#/settings', group: 'app' },
    { key: 'shell.footer.recover', href: '#/orders/recover', group: 'app' },
    { key: 'shell.footer.kaspa', href: 'https://kaspa.org', external: true, group: 'app' },
  ];
}

/**
 * The chart library's attribution ("Charts by TradingView Lightweight Charts™", its Apache-2.0 NOTICE and TradingView's condition): on every app page,
 * any of which can show a chart; not on the landing page, which has none.
 */
export const showsChartNotice = (context: FrameContext): boolean => context === 'app';
