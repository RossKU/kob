// The landing-only page (src/app/landing-only.ts): the production front of a network that has not launched. The landing content in its "in preparation" mode with
// its own footer (no link into the app), and nothing of the app around it: no services, no router, no wallet. It keeps the address on the page itself.
import { version as appVersion } from '../../../package.json';
import { t } from '../../i18n';
import { soonFooterLinks } from '../shell/frame-model';
import { FooterView } from '../shell/frame-parts';
import { LandingContent, toTop } from './LandingContent';
import { landingSoonText, siteName } from './landing-model';

export function LandingOnlyApp() {
  const site = siteName(typeof location === 'undefined' ? '' : location.hostname);
  return (
    <div class="app">
      <LandingContent
        network="mainnet"
        mode="soon"
        footer={<FooterView context="landing" net={landingSoonText(site)} links={soonFooterLinks()} versions={t('shell.footer.version', { app: appVersion })} brandOnClick={toTop} brandLabel={t('landing.toTop')} />}
      />
    </div>
  );
}
