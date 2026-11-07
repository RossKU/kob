// The landing page's facts that depend on the deployment (pure, tested in landing-model.test.ts): which network it names, which site it speaks
// for, where its links go. The page never claims a network the build is not configured for (no "testnet" on mainnet).
import type { AppConfig, NetworkName } from '../../config';
import type { Route } from '../../app/router';
import { resolveHome, usdTokenOf, type HomeTarget } from '../market/usd-model';
import { t } from '../../i18n';

/** The project's source repository (GitHub links of the landing page). */
export const REPO_URL = 'https://github.com/RossKU/kob';
/** Where "Run an executor" leads (the footer, the Support page). */
export const EXECUTOR_URL = REPO_URL;
/** The project's README on GitHub, where the donation address is published too. */
export const README_URL = `${REPO_URL}#readme`;
/** The feedback section's private security report: GitHub's private vulnerability reporting form. */
export const SECURITY_REPORT_URL = `${REPO_URL}/security/advisories/new`;
export const ISSUES_URL = `${REPO_URL}/issues`;
/** Where "Send a message on X" leads. Empty: the action is not shown. */
export const FEEDBACK_CONTACT_URL: string = 'https://x.com/kaspaorderbook';
/** The market list: "Open the app" and the footer's "App" (an in-app link, so a self-hosted copy stays on its own host). */
export const APP_HASH = '#/market';

/** The networks the sentences below exist for (one dictionary key each). */
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

/** The name the page uses for the site it is served from: its host name, or "This site" for a local or unnamed one (localhost, an IP address). */
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
    txNote: t(`landing.tx.note.${net}`),
    auditBody: t(`landing.trust.audit.body.${net}`),
    discLead: t(`landing.disc.lead.${net}`),
    discBody: `${t('landing.disc.body', { site })} ${t(`landing.disc.tail.${net}`)}`,
  };
}

/** The feedback section's actions: a security report, an issue and, once there is a channel, a message. */
export function feedbackActions(contactUrl: string = FEEDBACK_CONTACT_URL): { key: string; href: string; testid: string }[] {
  return [
    { key: 'landing.feedback.security', href: SECURITY_REPORT_URL, testid: 'landing-feedback-security' },
    { key: 'landing.feedback.issue', href: ISSUES_URL, testid: 'landing-feedback-issue' },
    ...(contactUrl ? [{ key: 'landing.feedback.message', href: contactUrl, testid: 'landing-feedback-message' }] : []),
  ];
}

/** The indexer's public health route ("Public API" in the footer), or null when the deployment has no indexer (the link is then not shown). */
export function publicApiUrl(indexerUrl: string | null | undefined): string | null {
  const base = (indexerUrl ?? '').trim().replace(/\/+$/, '');
  return /^https?:\/\//i.test(base) ? `${base}/v1/health` : null;
}

/**
 * What the empty hash shows, from config `home`: by default (`landing`) the landing page; an operator's explicit `home` (`auto`, `list`, `kas-usd`,
 * `usd:<id>`, `market:<id>`) a market view inside the app shell (usd-model.ts `resolveHome`). The market list is always `#/market`.
 */
export function homeTargetOf(config: Pick<AppConfig, 'home' | 'quoteTokens'>, tokens: readonly { covenantId: string; tradable: boolean }[]): HomeTarget {
  const usd = usdTokenOf(config.quoteTokens, tokens);
  return resolveHome(config.home, usd?.covenantId ?? null, tokens);
}

/** True when `route` shows the landing page: the empty hash with `home` = `landing`. It replaces the whole shell (header, banners, footer). */
export const showsLanding = (route: Route, config: Pick<AppConfig, 'home' | 'quoteTokens'>, tokens: readonly { covenantId: string; tradable: boolean }[]): boolean =>
  route.name === 'home' && homeTargetOf(config, tokens).name === 'landing';
