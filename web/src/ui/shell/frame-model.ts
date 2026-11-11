// The footer's facts (pure, tested in frame-model.test.ts): which links it shows on which page, and whether it carries the chart library's attribution.
import { TESTNET_APP_URL } from '../../app/landing-only';
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
  /** `site`: the links of the row (the landing page's, the app's compact line); `app`: kept for a divided group of further app links (none now). */
  group: 'site' | 'app';
}

/**
 * The footer's links. The landing page's full row: App, GitHub, Run an executor, when the deployment has an indexer its Public API, when there is a donation
 * address Support (`support`), and the Disclaimer. An app page has ONE compact line (review L-04) with only the essential ones: GitHub, Support (when there is
 * a donation address) and the Disclaimer; the security policy link is the footer's own (FooterView). The rest (App, Run an executor, Public API) is on the
 * landing page, and Settings is a tab of the header.
 */
export function footerLinks(context: FrameContext, indexerUrl: string | null | undefined, support = false): FooterLink[] {
  const api = publicApiUrl(indexerUrl);
  const supportLink = { key: 'landing.footer.support', href: '#/support', testid: 'footer-support', group: 'site' } as const;
  const disclaimer = { key: 'landing.footer.disclaimer', href: '#/disclaimer', testid: 'footer-disclaimer', group: 'site' } as const;
  if (context === 'app') return [{ key: 'landing.github', href: REPO_URL, group: 'site' }, ...(support ? [supportLink] : []), disclaimer];
  return [
    { key: 'landing.footer.app', href: APP_HASH, group: 'site' },
    { key: 'landing.github', href: REPO_URL, group: 'site' },
    { key: 'landing.footer.executor', href: EXECUTOR_URL, group: 'site' },
    ...(api ? [{ key: 'landing.footer.api', href: api, testid: 'landing-api', group: 'site' } as const] : []),
    ...(support ? [supportLink] : []),
    disclaimer,
  ];
}

/** The landing-only page's footer: the testnet app, GitHub and Run an executor; no link into an app that is not there (App, Mirror, Public API, Support, Disclaimer, Settings). */
export function soonFooterLinks(): FooterLink[] {
  return [
    { key: 'landing.soon.try', href: TESTNET_APP_URL, testid: 'footer-testnet-app', group: 'site' },
    { key: 'landing.github', href: REPO_URL, group: 'site' },
    { key: 'landing.footer.executor', href: EXECUTOR_URL, group: 'site' },
  ];
}

/**
 * The chart library's attribution ("Charts by TradingView Lightweight Charts™", its Apache-2.0 NOTICE and TradingView's condition): on every app page,
 * any of which can show a chart; not on the landing page, which has none.
 */
export const showsChartNotice = (context: FrameContext): boolean => context === 'app';
