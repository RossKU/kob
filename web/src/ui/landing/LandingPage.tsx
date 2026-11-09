// The landing page of a deployment with the app: what opens at the empty hash (config `home`, default `landing`) for a visitor arriving from outside. A full page
// in the app's frame (Frame.tsx: the same header row, here with GitHub, the network chip, "Open the app" -> `#/market` and the theme toggle, and the same footer)
// in place of the app's tabs, banners and views; every other route shows the app shell. The content is LandingContent.tsx (shared with the landing-only build).
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { SiteFooter } from '../shell/Frame';
import { LandingContent, toTop } from './LandingContent';

export function LandingPage() {
  const { config } = useServices();
  return <LandingContent network={config.network} mode="app" footer={<SiteFooter context="landing" brandOnClick={toTop} brandLabel={t('landing.toTop')} />} />;
}
