// Application shell: header, status bar, global banners, the routed view, footer, toasts.
import { useEffect } from 'preact/hooks';
import type { ConfigWarning, StoredOverride } from '../config';
import { t } from '../i18n';
import { IssueView } from '../ui/issue/IssueView';
import { ToastHost } from '../ui/kit';
import { MarketView } from '../ui/market/MarketView';
import { resolveHome, usdTokenOf } from '../ui/market/usd-model';
import { OrdersView } from '../ui/orders/OrdersView';
import { SettingsView } from '../ui/settings/SettingsView';
import { GlobalBanners } from '../ui/shell/Banners';
import { Footer } from '../ui/shell/Footer';
import { Header } from '../ui/shell/Header';
import { StatusBar } from '../ui/shell/StatusBar';
import { StatusProvider } from '../ui/shell/StatusProvider';
import { NotifyProvider } from '../ui/notify/NotifyProvider';
import { ViewBoundary } from '../ui/shell/ViewBoundary';
import { AutoRefundRunner } from '../ui/orders/AutoRefund';
import { useServices } from './context';
import { routeToHash, useRoute, usdMarketHash, type Route } from './router';

/** The landing screen (empty hash, config `home`): a chart by default; the market list stays at `#/market`. */
function HomeView() {
  const { config, registry } = useServices();
  const usd = usdTokenOf(config.quoteTokens, registry.tokens);
  const target = resolveHome(config.home, usd?.covenantId ?? null, registry.tokens);
  if (target.name === 'pair') return <MarketView pair={{ base: target.base, quote: target.quote }} />;
  if (target.name === 'token') return <MarketView covenantId={target.covenantId} />;
  return <MarketView />;
}

/**
 * Old `#/usd/kas` and `#/usd/<token>` links: there is no USD page, every USD view is a tradable page. They are redirected (the history entry is replaced) to
 * the USD token's market (<ticker>/KAS) or the pair page `<token>/<USD token>`; without a USD token or for an unknown token, to the market list.
 */
function LegacyUsdLink(props: { asset: string }) {
  const { config, registry } = useServices();
  const usd = usdTokenOf(config.quoteTokens, registry.tokens);
  const known = props.asset === 'kas' || registry.byCovenantId.has(props.asset);
  const hash = usd && known ? usdMarketHash(props.asset, usd.covenantId) : '#/market';
  useEffect(() => {
    window.location.replace(hash);
  }, [hash]);
  return null;
}

function RoutedView(props: { route: Route; warnings: ConfigWarning[] }) {
  const r = props.route;
  switch (r.name) {
    case 'home':
      return <HomeView />;
    case 'usd':
      return <LegacyUsdLink asset={r.asset} />;
    case 'market':
      return <MarketView invalidToken={r.invalidToken} />;
    case 'token':
      return <MarketView covenantId={r.covenantId} {...(r.ticket ? { ticket: r.ticket } : {})} {...(r.amount ? { amount: r.amount } : {})} />;
    case 'pair':
      return <MarketView pair={{ base: r.base, quote: r.quote }} />;
    case 'orders':
      return <OrdersView />;
    case 'issue':
      return <IssueView />;
    case 'settings':
      return <SettingsView warnings={props.warnings} />;
  }
}

export function App(props: { warnings: ConfigWarning[]; storedOverrides?: StoredOverride[] }) {
  const services = useServices();
  const route = useRoute();

  // the title names the app in the tab bar
  useEffect(() => {
    document.title = `${t('common.appName')} - ${t('common.tagline')}`;
  }, []);

  // one socket for the whole app: the shell subscribes to the global channels, views add their own (book:<token>, fills:<token>)
  useEffect(() => {
    const feed = services.feed;
    if (!feed) return;
    try {
      feed.subscribe(['health', 'reorg']);
      feed.connect();
    } catch {
      /* a browser without WebSocket: views fall back to polling */
    }
    return () => feed.close();
  }, [services.feed]);

  return (
    <div class="app">
      <NotifyProvider>
        <StatusProvider>
          <Header route={route} />
          <StatusBar />
          <main class="container main" id="main" data-testid="main" data-route={routeToHash(route)}>
            <GlobalBanners warnings={props.warnings} storedOverrides={props.storedOverrides ?? []} />
            <ViewBoundary resetKey={routeToHash(route)}>
              <RoutedView route={route} warnings={props.warnings} />
            </ViewBoundary>
          </main>
          <Footer />
        </StatusProvider>
      </NotifyProvider>
      <AutoRefundRunner />
      <ToastHost />
    </div>
  );
}
