// Entry point: config -> language -> boot screen -> services (wasm engine, SDK, node, indexer, registry) -> providers -> App.
// A failure anywhere in that chain shows a clear error page with details and a retry (never a blank page).
import { render } from 'preact';
import './styles.css';
import { App } from './app/App';
import { ServicesContext, WalletProvider } from './app/context';
import { createServices, type Services } from './app/services';
import { loadConfigWithWarnings, type ConfigWarning, type StoredOverride } from './config';
import { BootError, BootScreen } from './ui/shell/Boot';
import { initTheme } from './ui/shell/theme';

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
    const services = await createServices(config);
    if (config.features.test) window.__kob = { services };
    render(<Root services={services} warnings={warnings} storedOverrides={storedOverrides} />, root);
  } catch (e) {
    console.error('boot failed', e);
    render(<BootError error={e} onRetry={() => void boot()} />, root);
  }
}

void boot();
