// Application shell: header, status bar, global banners, the routed view, footer, toasts; or, at the empty hash by default, the landing page in its place.
import { useEffect, useRef } from 'preact/hooks';
import type { ConfigWarning, StoredOverride } from '../config';
import { t } from '../i18n';
import { IssueView } from '../ui/issue/IssueView';
import { ToastHost } from '../ui/kit';
import { MarketView } from '../ui/market/MarketView';
import { usdTokenOf } from '../ui/market/usd-model';
import { LandingPage } from '../ui/landing/LandingPage';
import { homeTargetOf, showsLanding } from '../ui/landing/landing-model';
import { OrdersView } from '../ui/orders/OrdersView';
import { SettingsView } from '../ui/settings/SettingsView';
import { GlobalBanners } from '../ui/shell/Banners';
import { SiteFooter, SkipLink } from '../ui/shell/Frame';
import { Header } from '../ui/shell/Header';
import { PreAuditBanner } from '../ui/shell/PreAuditBanner';
import { StatusProvider } from '../ui/shell/StatusProvider';
import { NotifyProvider } from '../ui/notify/NotifyProvider';
import { ViewBoundary } from '../ui/shell/ViewBoundary';
import { AutoRefundRunner } from '../ui/orders/AutoRefund';
import { StandaloneReceipts } from '../ui/shell/StandaloneReceipts';
import { useStandalone } from '../ui/shell/standalone';
import { useServices } from './context';
import { useDeclarationsVersion } from './use-declaration';
import { loadDeclaration, noteTokenProgram, restoreDeclarations } from '../data/token-declarations';
import { logoBlocklist, logoBlocklistUrlOf } from '../data/logo-blocklist';
import { routeToHash, useRoute, usdMarketHash, type Route } from './router';
import { appTitle, Titled, useDocumentTitle } from './page-title';

/**
 * The empty hash inside the shell: an operator's explicit config `home` (a chart, a pair or the list). The default `landing` never gets here: the App
 * draws the landing page in place of the whole shell (`showsLanding`). The market list stays at `#/market`.
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
      return (
        <Titled page="market">
          <MarketView invalidToken={r.invalidToken} />
        </Titled>
      );
    case 'token':
      return <MarketView covenantId={r.covenantId} {...(r.ticket ? { ticket: r.ticket } : {})} {...(r.amount ? { amount: r.amount } : {})} />;
    case 'pair':
      return <MarketView pair={{ base: r.base, quote: r.quote }} />;
    case 'orders':
      return (
        <Titled page="orders">
          <OrdersView />
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
  }
}

export function App(props: { warnings: ConfigWarning[]; storedOverrides?: StoredOverride[] }) {
  const services = useServices();
  const route = useRoute();

  // the tab's title names the page: each view sets its own (app/page-title.ts); only the legacy redirect, which shows no view, is left to the shell
  useDocumentTitle(route.name === 'usd' ? appTitle() : null);

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

  // KCC-23 declarations: the ones kept in this browser (re-verified), then those of the tokens the indexer marks as declared, and the program of
  // every listed token; every view re-renders when one becomes known (the names, tickers and decimals it shows follow: app/token-display.ts)
  useDeclarationsVersion();
  useEffect(() => {
    const { kob, indexer, config } = services;
    restoreDeclarations(kob);
    if (!indexer) return;
    let alive = true;
    indexer
      .tokens()
      .then((rows) => {
        // the program each token is listed under (a fact of its scripts): a token of a program with default decimals is shown with them
        // (KaspaRocket convention, app/token-display.ts)
        const tpls = kob.templates();
        for (const r of rows) {
          if (!alive) break;
          if (r.template_hash) noteTokenProgram(config.network, r.covenant_id, tpls.find((t) => t.hash === r.template_hash && t.tokenSlots)?.name);
          if (r.declaration) void loadDeclaration(kob, indexer, config.network, r.covenant_id);
        }
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [services]);

  // the token-logo blocklist (a logo that was reported is not loaded): read now and every 2 minutes. It fails OPEN: an indexer that cannot be reached, or has
  // no such route, leaves the last known list (empty on a first visit) and logos show (data/logo-blocklist.ts)
  const blocklistUrl = logoBlocklistUrlOf(services.config);
  useEffect(() => logoBlocklist.start(blocklistUrl), [blocklistUrl]);

  // one socket for the whole app: the shell subscribes to the global channels, views add their own (book:<token>, fills:<token>); none while standalone
  // mode is on by the setting (the indexer is not asked for the market)
  const manualStandalone = useStandalone(services.standalone)?.reason === 'manual';
  useEffect(() => {
    const feed = services.feed;
    if (!feed || manualStandalone) return;
    try {
      feed.subscribe(['health', 'reorg']);
      feed.connect();
    } catch {
      /* a browser without WebSocket: views fall back to polling */
    }
    return () => feed.close();
  }, [services.feed, manualStandalone]);

  // the empty hash with the default config `home`: the landing page, a full page in the shared frame (Frame.tsx: its own header row, the same
  // footer) in place of the whole shell (tabs, banners, views); every other route shows the shell
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
        <StandaloneReceipts />
        <ToastHost />
      </ViewBoundary>
    </div>
  );
}

/** The shell around every routed view: the skip link, the header, the global banners, the view and the footer. */
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
            <Header route={route} />
          </ViewBoundary>
          <main class="container main" id="main" tabIndex={-1} data-testid="main" data-route={routeKey}>
            <ViewBoundary resetKey={routeKey}>
              <PreAuditBanner />
              <GlobalBanners warnings={props.warnings} storedOverrides={props.storedOverrides} />
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
