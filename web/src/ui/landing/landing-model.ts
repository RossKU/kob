// The landing page's facts that depend on the deployment. They are pure and tested in
// landing-model.test.ts. They say which network the page names, which site it speaks for and
// where its links go.
// The page never claims a network the build is not configured for, so mainnet never says
// "testnet".
import type { AppConfig, NetworkName } from '../../config';
import type { Route } from '../../app/router';
import { resolveHome, usdTokenOf, type HomeTarget } from '../market/usd-model';
import { t } from '../../i18n';

/** The project's source repository, used by the GitHub links of the landing page. */
export const REPO_URL = 'https://github.com/RossKU/kob';
/** Where "Run an executor" leads, in the footer and the Support page. */
export const EXECUTOR_URL = REPO_URL;
/** The project's README on GitHub, which also publishes the donation address. */
export const README_URL = `${REPO_URL}#readme`;
/** The trust us row's private security report, GitHub's private vulnerability reporting form. */
export const SECURITY_REPORT_URL = `${REPO_URL}/security/advisories/new`;
export const ISSUES_URL = `${REPO_URL}/issues`;
/** Where "Send a message on X" leads. When it is empty, the action is not shown. */
export const FEEDBACK_CONTACT_URL: string = 'https://x.com/kaspaorderbook';
/**
 * The market list, for "Open the app" and the footer's "App". It is an in-app link, so a
 * self-hosted copy stays on its own host.
 */
export const APP_HASH = '#/market';


/** The networks the sentences below exist for, with one dictionary key each. */
export const LANDING_NETWORKS: readonly NetworkName[] = ['testnet-10', 'mainnet'];

/** The network-dependent sentences of the page. */
export interface LandingNetworkText {
  chip: string;
  chipTip: string;
  txNote: string;
  auditBody: string;
  discLead: string;
  discBody: string;
}

/**
 * The name the page uses for the site it is served from. It is the host name, or "This site" for
 * a local or unnamed one such as localhost or an IP address.
 */
export function siteName(hostname: string | null | undefined): string {
  const h = (hostname ?? '').trim().toLowerCase();
  const local = h === '' || h === 'localhost' || h.endsWith('.localhost') || /^[0-9.]+$/.test(h) || h.includes(':');
  return local ? t('landing.site') : h;
}

/** The sentences for `network`, naming `site`. */
export function landingNetworkText(network: NetworkName, site: string): LandingNetworkText {
  const net = (LANDING_NETWORKS as readonly string[]).includes(network) ? network : 'mainnet';
  return {
    chip: t('landing.chip', { network: net }),
    chipTip: t('landing.chipTip', { network: net }),
    txNote: t('landing.tx.note'),
    auditBody: t('landing.trust.audit.body'),
    discLead: t('landing.disc.lead'),
    discBody: `${t('landing.disc.body', { site })} ${t(`landing.disc.tail.${net}`)}`,
  };
}

/**
 * The trust us row's actions. They are a security report, an issue and, once there is a
 * channel, a message.
 */
export function feedbackActions(contactUrl: string = FEEDBACK_CONTACT_URL): { key: string; href: string; testid: string }[] {
  return [
    { key: 'landing.feedback.security', href: SECURITY_REPORT_URL, testid: 'landing-feedback-security' },
    { key: 'landing.feedback.issue', href: ISSUES_URL, testid: 'landing-feedback-issue' },
    ...(contactUrl ? [{ key: 'landing.feedback.message', href: contactUrl, testid: 'landing-feedback-message' }] : []),
  ];
}

/**
 * The indexer's public health route, shown as "Public API" in the footer. It is null when the
 * deployment has no indexer, and the link is then not shown.
 */
export function publicApiUrl(indexerUrl: string | null | undefined): string | null {
  const base = (indexerUrl ?? '').trim().replace(/\/+$/, '');
  return /^https?:\/\//i.test(base) ? `${base}/v1/health` : null;
}

/**
 * What the empty hash shows, from config `home`. By default (`landing`) it is the landing page.
 * An explicit `home` of an operator (`auto`, `list`, `kas-usd`, `usd:<id>` or `market:<id>`) gives
 * a market view inside the app shell (usd-model.ts `resolveHome`). The market list is always
 * `#/market`.
 */
export function homeTargetOf(config: Pick<AppConfig, 'home' | 'quoteTokens'>, tokens: readonly { covenantId: string; tradable: boolean }[]): HomeTarget {
  const usd = usdTokenOf(config.quoteTokens, tokens);
  return resolveHome(config.home, usd?.covenantId ?? null, tokens);
}

/**
 * True when `route` shows the landing page, which is the empty hash with `home` set to `landing`.
 * It replaces the whole shell of header, banners and footer.
 */
export const showsLanding = (route: Route, config: Pick<AppConfig, 'home' | 'quoteTokens'>, tokens: readonly { covenantId: string; tradable: boolean }[]): boolean =>
  route.name === 'home' && homeTargetOf(config, tokens).name === 'landing';
