// Application shell: header, status bar, global banners, the routed view, footer, toasts.
import { useEffect, useRef } from 'preact/hooks';
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
import { PreAuditBanner } from '../ui/shell/PreAuditBanner';
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

  // a new page moves the focus to its content (screen readers announce it, Tab continues from there), not on the first load and not from an open dialog
  const routeKey = routeToHash(route);
  const firstRoute = useRef(true);
  useEffect(() => {
    if (firstRoute.current) {
      firstRoute.current = false;
      return;
    }
    if (document.querySelector('[role="dialog"]')) return;
    document.getElementById('main')?.focus({ preventScroll: true });
  }, [routeKey]);

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
      {/* keyboard users jump past the header; a hash link would be a route here, so it moves the focus itself */}
      <a
        class="skip-link"
        href="#/"
        data-testid="skip-link"
        onClick={(e) => {
          e.preventDefault();
          document.getElementById('main')?.focus();
        }}
      >
        {t('shell.skip')}
      </a>
      <NotifyProvider>
        <StatusProvider>
          {/* the shell has boundaries too: a render error escaping to the root would leave every component queued behind it in that render
              unable to re-render ever again (Preact drops the rest of its queue), e.g. a button whose click changes nothing */}
          <ViewBoundary resetKey={routeKey}>
            <Header route={route} />
            <StatusBar />
          </ViewBoundary>
          <main class="container main" id="main" tabIndex={-1} data-testid="main" data-route={routeToHash(route)}>
            <ViewBoundary resetKey={routeKey}>
              <PreAuditBanner />
              <GlobalBanners warnings={props.warnings} storedOverrides={props.storedOverrides ?? []} />
            </ViewBoundary>
            <ViewBoundary resetKey={routeKey}>
              <RoutedView route={route} warnings={props.warnings} />
            </ViewBoundary>
          </main>
          <ViewBoundary resetKey={routeKey}>
            <Footer />
          </ViewBoundary>
        </StatusProvider>
      </NotifyProvider>
      <ViewBoundary resetKey={routeKey}>
        <AutoRefundRunner />
        <ToastHost />
      </ViewBoundary>
    </div>
  );
}
