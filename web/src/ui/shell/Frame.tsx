// The frame shared by the landing page and the app: one header row (SiteHeader) and one footer (SiteFooter), the brand lockup, the theme toggle and the
// skip link. Only the header's contents differ by page (the landing page: GitHub, the network chip, "Open the app", the theme toggle; the app: Header.tsx's
// tabs and tools). Its height, column, brand and controls are the same on both (styles.css, "frame"), so the logo stays put when "Open the app" is clicked.
// The pieces that need no services are in frame-parts.tsx (re-exported here); this file adds the footer that reads its facts from the services.
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { version as appVersion } from '../../../package.json';
import { indexerTrust } from '../../data/indexer-crosscheck';
import { landingNetworkText, siteName } from '../landing/landing-model';
import { supportAddressOf } from '../support/support-model';
import { footerLinks, type FrameContext } from './frame-model';
import { useFailoverState } from './failover';
import { FooterView, releasedVersion } from './frame-parts';

export * from './frame-parts';

/**
 * The footer: the landing page's is the full one (the brand, the tagline and one row of links; the network's disclaimer, the site publishes software and
 * information only, with the security policy; the versions). An app page has ONE compact line (FooterView): the brand, the version, the open review lead, the
 * Disclaimer, the security policy and GitHub, the indexer trust and the chart library's attribution (TradingView's Apache-2.0 NOTICE, required on every page that
 * can show its charts; the landing page has none).
 */
export function SiteFooter(props: { context: FrameContext; brandOnClick?: (e: Event) => void; brandLabel?: string }) {
  const services = useServices();
  const { config, kob, indexer } = services;
  useFailoverState(services.failover); // re-render on a switch: the verifiers are those of the indexer in use
  const app = props.context === 'app';
  const net = landingNetworkText(config.network, siteName(typeof location === 'undefined' ? '' : location.hostname));
  const links = footerLinks(props.context, config.indexerUrl, !!supportAddressOf(config.features));
  const trust = indexerTrust(!!indexer, services.verifiers?.length ?? 0);
  let kobVersion = '?';
  if (app) {
    try {
      kobVersion = kob.version();
    } catch {
      /* an unusable engine is reported by the boot screen, not here */
    }
  }
  // an unreportable or unreleased engine build (0.0.0): no engine version to show
  const versions = app && releasedVersion(kobVersion) ? t('shell.footer.versions', { app: appVersion, kob: kobVersion }) : t('shell.footer.version', { app: appVersion });
  return <FooterView context={props.context} net={net} links={links} versions={versions} brandOnClick={props.brandOnClick} brandLabel={props.brandLabel} {...(app ? { trust } : {})} />;
}

