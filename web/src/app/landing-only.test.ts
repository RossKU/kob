// The landing-only mode (KOB_LANDING_ONLY, app/landing-only.ts): the build flag, the router guard (every address is the landing page, the address is cleaned), the
// wiring of the build, and the bundle boundary: the landing-only entry's import graph has none of the app in it.
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { LANDING_CSP_META, LANDING_META_DIRECTIVES, META_DIRECTIVES } from '../../csp.mjs';
import { cleanLandingUrl, keepLandingUrl, LANDING_ONLY, parseLandingOnly, TESTNET_APP_URL } from './landing-only';
import { parseHash } from './router';

const WEB = fileURLToPath(new URL('../../', import.meta.url));
const read = (rel: string) => readFileSync(join(WEB, rel), 'utf8');

describe('KOB_LANDING_ONLY build flag', () => {
  it('is off unless set to 1 / true; a typo fails the build instead of building the app', () => {
    for (const off of [undefined, '', '  ', '0', 'false', 'FALSE']) expect(parseLandingOnly(off), String(off)).toBe(false);
    for (const on of ['1', 'true', ' True ', '1\n']) expect(parseLandingOnly(on), on).toBe(true);
    for (const bad of ['yes', 'on', '2', 'landing', 'landing-only']) expect(() => parseLandingOnly(bad), bad).toThrow(/KOB_LANDING_ONLY/);
  });

  it('the bundle under test is the app (vitest defines __KOB_LANDING_ONLY__ as false)', () => {
    expect(LANDING_ONLY).toBe(false);
  });

  it('vite.config.ts parses the flag, defines it, and swaps the entry only for a landing-only build', () => {
    const cfg = read('vite.config.ts');
    expect(cfg).toContain('parseLandingOnly(process.env.KOB_LANDING_ONLY)');
    expect(cfg).toContain('__KOB_LANDING_ONLY__: JSON.stringify(landingOnly)');
    expect(cfg).toContain('...(landingOnly ? [kobLandingOnly()] : [])');
    expect(cfg).toContain("html.replace('/src/main.tsx', '/src/main-landing.tsx')");
    // no signer page in the build (it is where private keys are typed; nothing on the front page signs)
    expect(cfg).toContain("...(landingOnly ? {} : { signer: fileURLToPath(new URL('./signer.html', import.meta.url)) })");
    // the start URL of the installed page is the page, not the market list; the CSP is the one with no network
    expect(cfg).toContain("manifest.start_url = './'");
    expect(cfg).toContain('landingOnly ? LANDING_CSP_META : CSP_META');
    expect(read('index.html')).toContain('src="/src/main.tsx"'); // the default entry the plugin swaps
  });

  it('the app\'s manifest still opens the market (the testnet build is unchanged); only the landing-only build rewrites it', () => {
    expect((JSON.parse(read('public/manifest.webmanifest')) as { start_url: string }).start_url).toBe('./#/market');
  });
});

describe('the landing-only router guard: every address is the landing page', () => {
  const at = (hash: string) => cleanLandingUrl({ pathname: '/', search: '', hash });
  const A = 'a'.repeat(64);

  it('the page itself (no hash, #, #/) is left alone', () => {
    for (const h of ['', '#', '#/']) expect(at(h), h).toBeNull();
  });

  it('every app route, and any other hash, is cleaned to the page: #/market, #/token/.., #/pair/.., #/activity, #/orders, #/settings ...', () => {
    for (const h of ['#/market', `#/market/${A}`, `#/token/${A}`, `#/pair/${A}/${'b'.repeat(64)}`, '#/activity', '#/orders', '#/create', '#/settings', '#/disclaimer', '#/support', '#/usd/kas', '#/nope', '#anything']) {
      expect(at(h), h).toBe('/');
    }
    // every route the app knows is among them (a new app route needs no change here: any hash but the page's own is cleaned)
    for (const h of ['#/market', '#/orders', '#/settings']) expect(parseHash(h).name).not.toBe('home');
  });

  it('keeps the path and the query (a project site under a sub-path, a campaign query) and drops only the hash', () => {
    expect(cleanLandingUrl({ pathname: '/kob/', search: '?ref=x', hash: '#/orders' })).toBe('/kob/?ref=x');
  });

  it('keepLandingUrl cleans now and after every hash change, and stops on request', () => {
    const calls: string[] = [];
    const listeners = new Map<string, () => void>();
    const win = {
      location: { pathname: '/', search: '', hash: '#/market' },
      history: { state: null, replaceState: (_s: unknown, _t: string, url: string) => (calls.push(url), (win.location.hash = '')) },
      addEventListener: (n: string, f: () => void) => listeners.set(n, f),
      removeEventListener: (n: string) => listeners.delete(n),
    };
    const stop = keepLandingUrl(win as unknown as Window);
    expect(calls).toEqual(['/']); // at once
    win.location.hash = '#/orders';
    listeners.get('hashchange')!();
    expect(calls).toEqual(['/', '/']);
    win.location.hash = '#/';
    listeners.get('hashchange')!();
    expect(calls).toHaveLength(2); // the page's own hash is kept
    stop();
    expect(listeners.size).toBe(0);
  });

  it('the way forward is the testnet-10 app at its market list, on its own host', () => {
    expect(TESTNET_APP_URL).toBe('https://kob-tn10.kaspa.ag/#/market');
    expect(parseHash(new URL(TESTNET_APP_URL).hash)).toEqual({ name: 'market' });
  });
});

describe('the landing-only CSP', () => {
  it('is the app policy with no network, no frames and no pictures but the page\'s own', () => {
    expect(LANDING_META_DIRECTIVES['connect-src']).toEqual(["'none'"]);
    expect(LANDING_META_DIRECTIVES['frame-src']).toEqual(["'none'"]);
    expect(LANDING_META_DIRECTIVES['img-src']).toEqual(["'self'", 'data:']);
    // nothing else is looser than the app's
    for (const [k, v] of Object.entries(LANDING_META_DIRECTIVES)) {
      if (['connect-src', 'frame-src', 'img-src'].includes(k)) continue;
      expect(v, k).toEqual(META_DIRECTIVES[k]);
    }
    expect(Object.keys(LANDING_META_DIRECTIVES).sort()).toEqual(Object.keys(META_DIRECTIVES).sort());
    expect(LANDING_CSP_META).toContain("connect-src 'none'");
    expect(LANDING_CSP_META).not.toMatch(/frame-ancestors/); // header only, from public/_headers
  });
});

// ------------------------------------------------------------------------------------------------ the bundle boundary

/** The files `file` imports at run time (static `import ... from` / `import '...'` / dynamic `import('...')`; `import type` is erased by the build), resolved inside src/; packages and assets are skipped. */
function importsOf(file: string): string[] {
  const text = readFileSync(file, 'utf8').replace(/(^|\n)\s*(?:import|export)\s+type\b[^;]*;/g, '$1');
  const specs = [...text.matchAll(/(?:^|\n)\s*(?:import|export)\s[^'"\n]*?from\s+['"]([^'"]+)['"]|(?:^|\n)\s*import\s+['"]([^'"]+)['"]|import\(\s*['"]([^'"]+)['"]\s*\)/g)].map((m) => m[1] ?? m[2] ?? m[3]!);
  const out: string[] = [];
  for (const s of specs) {
    if (!s.startsWith('.')) continue;
    const base = resolve(dirname(file), s);
    const found = [base, `${base}.ts`, `${base}.tsx`, join(base, 'index.ts'), join(base, 'index.tsx')].find((p) => existsSync(p) && statSync(p).isFile());
    if (found && /\.(ts|tsx)$/.test(found)) out.push(found);
  }
  return out;
}

function graphOf(entry: string): string[] {
  const seen = new Set<string>();
  const walk = (f: string) => {
    if (seen.has(f)) return;
    seen.add(f);
    for (const i of importsOf(f)) walk(i);
  };
  walk(entry);
  return [...seen].map((f) => relative(join(WEB, 'src'), f).replace(/\\/g, '/')).sort();
}

describe('the landing-only entry (src/main-landing.tsx) is the landing page and nothing of the app', () => {
  const graph = graphOf(join(WEB, 'src/main-landing.tsx'));

  it('has none of the router, config, services, wallet layer, wasm engine, indexer client or trading views in its import graph', () => {
    const forbidden = [
      /^app\/(router|context|services|App|env|commit|book-|pair-book|pending-spends|wallet-events|fingerprint|explorer|use-declaration|token-)/,
      /^config\.ts$/,
      /^main\.tsx$/,
      /^wallet\//,
      /^data\//,
      /^kob\/(?!(units|pair)\.ts$)/, // the engine (wasm), the planners, the registry, the records: none; two pure bigint helpers of the formatting code are (no DOM, no I/O)
      /^signer\//,
      /^ui\/(activity|issue|orders|settings|ticket|confirm|notify|support)\//,
      /^ui\/market\/(?!(usd-model|market-model)\.ts$)/, // the views and charts: none; their two pure models are (formatting helpers of the landing model)
      /^ui\/shell\/(Header|Boot|Banners|StatusProvider|SignerFrame|DisclaimerPage|failover|standalone)/,
    ];
    const bad = graph.filter((f) => forbidden.some((re) => re.test(f)));
    expect(bad).toEqual([]);
  });

  it('is the landing content, the frame pieces and the dictionary, a few dozen files', () => {
    for (const f of ['main-landing.tsx', 'app/landing-only.ts', 'ui/landing/LandingOnly.tsx', 'ui/landing/LandingContent.tsx', 'ui/shell/frame-parts.tsx', 'i18n/index.ts']) expect(graph).toContain(f);
    expect(graph.length).toBeLessThan(60);
  });

  it('the app entry still reaches the whole app (the graph check is not vacuous)', () => {
    const app = graphOf(join(WEB, 'src/main.tsx'));
    for (const f of ['app/App.tsx', 'app/router.ts', 'app/services.ts', 'config.ts', 'ui/shell/Frame.tsx']) expect(app).toContain(f);
    expect(app.length).toBeGreaterThan(graph.length * 5);
    expect(app).not.toContain('main-landing.tsx');
  });

  it('main-landing.tsx reads no config and starts no connection: no fetch, WebSocket, XMLHttpRequest, serviceWorker, config.json', () => {
    for (const f of graph.filter((g) => !g.startsWith('i18n/'))) {
      const text = readFileSync(join(WEB, 'src', f), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
      expect(text, f).not.toMatch(/\bfetch\s*\(|new\s+WebSocket|XMLHttpRequest|serviceWorker|sendBeacon|config\.json|__KOB_CONFIG__/);
    }
  });

  it('the app\'s main.tsx does not know the landing-only mode (the testnet build is unchanged)', () => {
    expect(read('src/main.tsx')).not.toMatch(/landing-only|LANDING_ONLY/);
    expect(read('src/app/App.tsx')).not.toMatch(/landing-only|LANDING_ONLY/);
  });
});
