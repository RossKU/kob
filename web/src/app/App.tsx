// The application shell. It holds the header, the status bar, the wrong-network banner, the routed
// view, the footer and the toasts. At the empty hash, by default, the landing page takes its place.
import { useEffect, useRef } from 'preact/hooks';
import type { ConfigWarning, StoredOverride } from '../config';
import { t } from '../i18n';
import { ActivityPage } from '../ui/activity/ActivityPage';
import { IssueView } from '../ui/issue/IssueView';
import { ToastHost } from '../ui/kit';
import { MarketView } from '../ui/market/MarketView';
import { LandingPage } from '../ui/landing/LandingPage';
import { homeTargetOf, showsLanding } from '../ui/landing/landing-model';
import { OrdersView } from '../ui/orders/OrdersView';
import { SettingsView } from '../ui/settings/SettingsView';
import { NetworkMismatchBanner } from '../ui/shell/Banners';
import { DisclaimerPage } from '../ui/shell/DisclaimerPage';
import { SiteFooter, SkipLink } from '../ui/shell/Frame';
import { Header } from '../ui/shell/Header';
import { StatusProvider } from '../ui/shell/StatusProvider';
import { NotifyProvider } from '../ui/notify/NotifyProvider';
import { ViewBoundary } from '../ui/shell/ViewBoundary';
import { AutoRefundRunner } from '../ui/orders/AutoRefund';
import { StandaloneReceipts } from '../ui/shell/StandaloneReceipts';
import { SupportPage } from '../ui/support/SupportPage';
import { routeWithSupport, supportAddressOf } from '../ui/support/support-model';
import { useStandalone } from '../ui/shell/standalone';
import { useServices } from './context';
import { KasPriceFeed } from './use-fiat';
import { useDeclarationsVersion } from './use-declaration';
import { loadDeclaration, noteListedTickers, noteMarketDecimals, noteTokenProgram, restoreDeclarations } from '../data/token-declarations';
import { exponentOf } from '../kob/open-token';
import { logoBlocklist, logoBlocklistUrlOf } from '../data/logo-blocklist';
import { routeToHash, useRoute, type Route } from './router';
import { Titled } from './page-title';

/**
 * The empty hash inside the shell, which shows an operator's explicit config `home`: a chart, a
 * pair or the list. The default `landing` never gets here, because the App draws the landing page
 * in place of the whole shell (`showsLanding`). The market list stays at `#/market`.
 */
function HomeView() {
  const { config, registry } = useServices();
  const target = homeTargetOf(config, registry.tokens);
  if (target.name === 'pair') return <MarketView pair={{ base: target.base, quote: target.quote }} />;
  if (target.name === 'token') return <MarketView covenantId={target.covenantId} />;
  return (
    <Titled page="market">
      <MarketView />
    </Titled>
  );
}

function RoutedView(props: { route: Route; warnings: ConfigWarning[] }) {
  const r = props.route;
  switch (r.name) {
    case 'home':
      return <HomeView />;
    case 'market':
      return (
        <Titled page="market">
          <MarketView invalidToken={r.invalidToken} />
        </Titled>
      );
    case 'token':
      return <MarketView covenantId={r.covenantId} {...(r.ticket ? { ticket: r.ticket } : {})} {...(r.amount ? { amount: r.amount } : {})} {...(r.price ? { price: r.price } : {})} />;
    case 'pair':
      return <MarketView pair={{ base: r.base, quote: r.quote }} />;
    case 'activity':
      return (
        <Titled page="activity">
          <ActivityPage />
        </Titled>
      );
    case 'portfolio':
      return (
        <Titled page="portfolio">
          <OrdersView {...(r.tab ? { tab: r.tab } : {})} {...(r.recover ? { recover: true } : {})} />
        </Titled>
      );
    case 'create':
      return (
        <Titled page="create">
          <IssueView />
        </Titled>
      );
    case 'settings':
      return (
        <Titled page="settings">
          <SettingsView warnings={props.warnings} />
        </Titled>
      );
    case 'disclaimer':
      return (
        <Titled page="disclaimer">
          <DisclaimerPage />
        </Titled>
      );
    case 'support':
      return (
        <Titled page="support">
          <SupportPage />
        </Titled>
      );
  }
}

/** The app: the landing page or the shell for the route, and the runners every page shares. */
export function App(props: { warnings: ConfigWarning[]; storedOverrides?: StoredOverride[] }) {
  const services = useServices();
  const route = routeWithSupport(useRoute(), supportAddressOf(services.config.features));

  // A new page moves the focus to its content, so screen readers announce it and Tab continues from
  // there. This does not happen on the first load or from an open dialog.
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

  // KCC-23 declarations are loaded here: the ones kept in this browser, verified again, then those
  // of the tokens the indexer marks as declared, and the program of every listed token. Every view
  // re-renders when one becomes known, and the names, tickers and decimals it shows follow
  // (app/token-display.ts).
  useDeclarationsVersion();
  useEffect(() => {
    const { kob, indexer, config } = services;
    restoreDeclarations(kob);
    if (!indexer) return;
    let alive = true;
    indexer
      .tokens()
      .then((rows) => {
        // The program each token is listed under, a fact of its scripts. A token of a program with
        // default decimals is shown with them (KaspaRocket convention, app/token-display.ts).
        const tpls = kob.templates();
        for (const r of rows) {
          if (!alive) break;
          if (r.template_hash) noteTokenProgram(config.network, r.covenant_id, tpls.find((t) => t.hash === r.template_hash && t.tokenSlots)?.name);
          // The token's scale, else the one its first order set. Its amounts are read on it where
          // neither a declaration nor a program states its decimals.
          const scale = r.scale ?? r.market_scale;
          if (typeof scale === 'number') noteMarketDecimals(config.network, r.covenant_id, exponentOf(scale));
          if (r.declaration) void loadDeclaration(kob, indexer, config.network, r.covenant_id);
        }
        if (alive) noteListedTickers(config.network, rows);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [services]);

  // The token-logo blocklist, so a logo that was reported is not loaded. It is read now and every 2
  // minutes. It fails open. An indexer that cannot be reached, or has no such route, leaves the
  // last known list, which is empty on a first visit, and logos show (data/logo-blocklist.ts).
  const blocklistUrl = logoBlocklistUrlOf(services.config);
  useEffect(() => logoBlocklist.start(blocklistUrl), [blocklistUrl]);

  // One socket for the whole app. The shell subscribes to the global channels, and views add their
  // own (book:<token>, fills:<token>). There is none while the setting turns standalone mode on,
  // because the indexer is then not asked for the market.
  const manualStandalone = useStandalone(services.standalone)?.reason === 'manual';
  useEffect(() => {
    const feed = services.feed;
    if (!feed || manualStandalone) return;
    try {
      feed.subscribe(['health', 'reorg']);
      feed.connect();
    } catch {
      /* A browser without WebSocket. Views fall back to polling. */
    }
    return () => feed.close();
  }, [services.feed, manualStandalone]);

  // The empty hash with the default config `home` shows the landing page in place of the whole
  // shell of tabs, bell and views. It is a full page in the shared frame (Frame.tsx), with its own
  // header row and the same footer. Every other route shows the shell.
  const landing = showsLanding(route, services.config, services.registry.tokens);

  return (
    <div class="app">
      {landing ? (
        <ViewBoundary resetKey={routeKey}>
          <LandingPage />
        </ViewBoundary>
      ) : (
        <AppShell route={route} routeKey={routeKey} warnings={props.warnings} storedOverrides={props.storedOverrides ?? []} />
      )}
      <ViewBoundary resetKey={routeKey}>
        <AutoRefundRunner />
        <KasPriceFeed />
        <StandaloneReceipts />
        <ToastHost />
      </ViewBoundary>
    </div>
  );
}

/**
 * The shell around every routed view. It holds the skip link, the header whose bell lists the
 * current status, the wrong-network banner, the view and the footer.
 */
function AppShell(props: { route: Route; routeKey: string; warnings: ConfigWarning[]; storedOverrides: StoredOverride[] }) {
  const { route, routeKey } = props;
  return (
    <>
      {/* keyboard users jump past the header */}
      <SkipLink />
      <NotifyProvider>
        <StatusProvider>
          {/* the shell has boundaries too: a render error escaping to the root would leave every component queued behind it in that render
              unable to re-render ever again (Preact drops the rest of its queue), e.g. a button whose click changes nothing */}
          <ViewBoundary resetKey={routeKey}>
            <Header route={route} warnings={props.warnings} storedOverrides={props.storedOverrides} />
          </ViewBoundary>
          <main class="container main" id="main" tabIndex={-1} data-testid="main" data-route={routeKey}>
            <ViewBoundary resetKey={routeKey}>
              <NetworkMismatchBanner />
            </ViewBoundary>
            <ViewBoundary resetKey={routeKey}>
              <RoutedView route={route} warnings={props.warnings} />
            </ViewBoundary>
          </main>
          <ViewBoundary resetKey={routeKey}>
            <SiteFooter context="app" />
          </ViewBoundary>
        </StatusProvider>
      </NotifyProvider>
    </>
  );
}
