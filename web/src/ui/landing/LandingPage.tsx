// The landing page of a deployment with the app. It opens at the empty hash for a visitor
// arriving from outside, when config `home` is `landing`, which is the default.
// It is a full page in the app's frame (Frame.tsx), in place of the app's tabs, banners and views.
// The frame has the same header row, here with GitHub, the network chip, "Open the app" (to
// `#/market`) and the theme toggle, and the same footer. Every other route shows the app shell.
// The content is LandingContent.tsx.
import { useServices } from '../../app/context';
import { t } from '../../i18n';
import { SiteFooter } from '../shell/Frame';
import { LandingContent, toTop } from './LandingContent';

export function LandingPage() {
  const { config } = useServices();
  return <LandingContent network={config.network} footer={<SiteFooter context="landing" brandOnClick={toTop} brandLabel={t('landing.toTop')} />} />;
}
