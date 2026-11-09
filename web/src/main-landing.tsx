// Entry point of the landing-only build (KOB_LANDING_ONLY, src/app/landing-only.ts; vite.config.ts serves this file instead of src/main.tsx): it draws the landing
// page and nothing else. There is no config, no services, no wasm engine, no wallet, no node or indexer connection and no router: every address shows the page.
import { render } from 'preact';
import './styles.css';
import { keepLandingUrl } from './app/landing-only';
import { installTranslationSafety } from './app/translation-safe';
import { LandingOnlyApp } from './ui/landing/LandingOnly';
import { initTheme } from './ui/shell/theme';

// a machine-translated page must not keep showing stale texts (app/translation-safe.ts)
installTranslationSafety();
initTheme();
keepLandingUrl(window);
render(<LandingOnlyApp />, document.getElementById('app') as HTMLElement);
