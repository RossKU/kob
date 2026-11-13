// Entry point: config -> language -> boot screen -> services (wasm engine, SDK, node, indexer, registry) -> providers -> App.
// A failure anywhere in that chain shows a clear error page with details and a retry (never a blank page).
import { render } from 'preact';
import './styles.css';
import { App } from './app/App';
import { ServicesContext, WalletProvider } from './app/context';
import { createServices, type Services } from './app/services';
import { installTokenAmounts } from './app/token-display';
import { applyNetworkToHead } from './app/page-title';
import { loadConfigWithWarnings, type ConfigWarning, type StoredOverride } from './config';
import { BootError, BootScreen } from './ui/shell/Boot';
import { initTheme } from './ui/shell/theme';
import { installTrustedTypesPolicy } from './app/trusted-types';
import { installTranslationSafety } from './app/translation-safe';
import { frameGuardOn, isFramed, showFrameGuard } from './app/mirror';
import { t } from './i18n';

// before anything renders: the only Trusted Types policy (csp.mjs), letting just the chart's attribution logo reach a DOM sink
installTrustedTypesPolicy();
// a machine-translated page must not keep showing stale texts (app/translation-safe.ts)
installTranslationSafety();

declare global {
  interface Window {
    /** test hook, present only when `features.test` is on */
    __kob?: { services: Services };
  }
}

const root = document.getElementById('app') as HTMLElement;

function Root(props: { services: Services; warnings: ConfigWarning[]; storedOverrides: StoredOverride[] }) {
  return (
    <ServicesContext.Provider value={props.services}>
      <WalletProvider services={props.services}>
        <App warnings={props.warnings} storedOverrides={props.storedOverrides} />
      </WalletProvider>
    </ServicesContext.Provider>
  );
}

async function boot(): Promise<void> {
  initTheme();
  render(<BootScreen />, root);
  try {
    const { config, warnings, storedOverrides } = await loadConfigWithWarnings();
    applyNetworkToHead(config.network);
    const services = await createServices(config);
    // amounts of tokens in sentences: whole tokens, as the pages show them (never "base units")
    installTokenAmounts(services.registry, config.network);
    if (config.features.test) window.__kob = { services };
    render(<Root services={services} warnings={warnings} storedOverrides={storedOverrides} />, root);
  } catch (e) {
    console.error('boot failed', e);
    render(<BootError error={e} onRetry={() => void boot()} />, root);
  }
}

// a mirror whose host cannot send frame-ancestors (app/mirror.ts): inside a frame, the page shows only a notice and a link to open it on its own
if (frameGuardOn() && isFramed()) {
  initTheme();
  showFrameGuard(document, root, location.href, { title: t('shell.frame.title'), body: t('shell.frame.body'), link: t('shell.frame.link') });
} else {
  void boot();
}
