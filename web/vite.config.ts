import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { defineConfig, type Plugin } from 'vite';
import { DEFAULT_REGISTRY_PATH } from './registry-pin.mjs';
import { rowsOfRegistryJson, tableHash } from './src/signer/table';
import preact from '@preact/preset-vite';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { CSP_META, LANDING_CSP_META, SIGNER_CSP_HEADER, SIGNER_CSP_META } from './csp.mjs';
import { applyHead, LANDING_ONLY_DESCRIPTION } from './head-meta.mjs';
import { parseLandingOnly } from './src/app/landing-only';
import { parseMirror } from './src/app/mirror';

// The landing-only mode (src/app/landing-only.ts): KOB_LANDING_ONLY=1 builds the production front page of a network that has not launched. Unknown values fail the build.
const landingOnly = parseLandingOnly(process.env.KOB_LANDING_ONLY);

const isSignerPage = (path: string) => /(^|\/)signer\.html$/.test(path);

// Embeds the Content-Security-Policy (csp.mjs) into the BUILT pages only (index.html: the app's; signer.html: the private-key signer's own,
// stricter one): the dev server needs inline scripts for hot reload.
const kobCsp = () => ({
  name: 'kob-csp',
  apply: 'build' as const,
  transformIndexHtml: (_html: string, ctx: { path: string }) => [
    { tag: 'meta', attrs: { 'http-equiv': 'Content-Security-Policy', content: isSignerPage(ctx.path) ? SIGNER_CSP_META : landingOnly ? LANDING_CSP_META : CSP_META }, injectTo: 'head-prepend' as const },
  ],
});

// The link-preview head of the app's page (title prefix of a testnet build, og:url and og:image): see head-meta.mjs. KOB_NETWORK and KOB_PUBLIC_URL
// are read when the build starts; unset, index.html is emitted as written. The signer page has none of it.
const kobHead = (): Plugin => ({
  name: 'kob-head',
  transformIndexHtml: {
    order: 'pre',
    handler: (html: string, ctx: { path: string }) => (isSignerPage(ctx.path) ? html : applyHead(html, { network: process.env.KOB_NETWORK, publicUrl: process.env.KOB_PUBLIC_URL, landingOnly })),
  },
});

// Landing-only (KOB_LANDING_ONLY): the page loads its own entry, src/main-landing.tsx (the landing page and nothing else), instead of src/main.tsx, so the router, the
// services, the wallet layer, the wasm engine and the trading views are not in the bundle at all; and the web app manifest opens the site at `./` (not the market list) with the "in preparation" description.
const kobLandingOnly = (): Plugin => {
  let outDir = '';
  return {
    name: 'kob-landing-only',
    configResolved: (c) => {
      outDir = resolve(c.root, c.build.outDir);
    },
    transformIndexHtml: {
      order: 'pre',
      handler: (html: string, ctx: { path: string }) => (isSignerPage(ctx.path) ? html : html.replace('/src/main.tsx', '/src/main-landing.tsx')),
    },
    closeBundle: {
      order: 'post',
      handler() {
        const file = resolve(outDir, 'manifest.webmanifest');
        const manifest = JSON.parse(readFileSync(file, 'utf8')) as Record<string, unknown>;
        manifest.start_url = './';
        manifest.description = LANDING_ONLY_DESCRIPTION;
        writeFileSync(file, JSON.stringify(manifest, null, 2) + '\n');
      },
    },
  };
};

// `vite preview` (the e2e target) serves what public/_headers asks of a production host for the signer: its page's header policy (with
// `frame-ancestors 'self'`), and CORS on /assets/*: the signer runs in an opaque origin (sandboxed frame), so its module scripts, stylesheet and
// wasm are cross-origin requests to the app's own host.
const kobSignerHeaders = (): Plugin => ({
  name: 'kob-signer-headers',
  configurePreviewServer(server) {
    server.middlewares.use((req, res, next) => {
      const url = (req.url ?? '').split('?')[0]!;
      if (url === '/signer' || isSignerPage(url)) {
        res.setHeader('Content-Security-Policy', SIGNER_CSP_HEADER);
        res.setHeader('X-Frame-Options', 'SAMEORIGIN');
      }
      if (url.startsWith('/assets/')) res.setHeader('Access-Control-Allow-Origin', '*');
      next();
    });
  },
});

// The bundler numbers the files it emits (here the two .wasm assets: kaspa_bg.wasm of the SDK and kob_wasm_bg.wasm) in the order their
// modules finish loading, which is a race between the two dynamically imported loaders. The number becomes the reference id of the
// placeholder `import.meta.ROLLDOWN_FILE_URL_<id>` that the asset URL replaces in the code, but the source maps keep it (in `names`, and in
// `sourcesContent` of the `?url` modules), so two builds of one commit could give maps where the two ids are swapped (CI job `reproducible`).
// Each id is replaced in the maps by one derived from the emitted file's own content-hashed name, of the same length (no column moves).
const kobStableMapRefs = (): Plugin => ({
  name: 'kob-stable-map-refs',
  apply: 'build',
  generateBundle(_options, bundle) {
    const stable = new Map<string, string>();
    const replace = (id: string) => {
      let s = stable.get(id);
      if (s === undefined) {
        s = createHash('sha256').update(this.getFileName(id)).digest('base64url').slice(0, id.length);
        stable.set(id, s);
      }
      return s;
    };
    for (const [fileName, out] of Object.entries(bundle)) {
      if (!fileName.endsWith('.map') || out.type !== 'asset') continue;
      const text = typeof out.source === 'string' ? out.source : new TextDecoder().decode(out.source);
      out.source = text.replace(/(ROLLDOWN_FILE_URL_)([A-Za-z0-9_-]+)/g, (_m, p: string, id: string) => p + replace(id));
    }
  },
});

// The registry table the signer frame pins (src/signer/table.ts, audit 2026-10-30 G7): the covenant id, ticker and decimals of the tokens of
// registry/tokens.json, or of the file `KOB_SIGNER_REGISTRY` names (a deployment that serves its own registry builds with that path: a signer
// whose table differs from the served one shows every token as unverified).
const registryTableHash = tableHash(rowsOfRegistryJson(JSON.parse(readFileSync(process.env.KOB_SIGNER_REGISTRY || DEFAULT_REGISTRY_PATH, 'utf8'))));

// The hosting mirror (src/app/mirror.ts): KOB_MIRROR=github-pages builds the GitHub Pages fallback (script frame guard, no private-key signing).
// Unset for every other build. An unknown value fails the build.
const mirror = parseMirror(process.env.KOB_MIRROR);

// Static, client-side build: `vite build` -> dist/ (serve from any static host; relative asset URLs).
export default defineConfig({
  base: './',
  define: { __KOB_REGISTRY_TABLE_HASH__: JSON.stringify(registryTableHash), __KOB_MIRROR__: JSON.stringify(mirror), __KOB_LANDING_ONLY__: JSON.stringify(landingOnly) },
  plugins: [preact(), kobHead(), kobCsp(), kobStableMapRefs(), kobSignerHeaders(), ...(landingOnly ? [kobLandingOnly()] : [])],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // keepNames: the official kaspa-wasm SDK identifies its JS classes (Transaction, TransactionOutput, Hash ...) by their `name` when it converts
  // request objects (rpc.submitTransaction: "Error converting property `covenant`" once a minifier renamed them). Found by the real-wallet TN10 run.
  // two pages: the app (index.html) and the private-key signer it frames (signer.html, src/signer)
  build: {
    target: 'es2022',
    sourcemap: true,
    chunkSizeWarningLimit: 4000,
    rolldownOptions: {
      // landing-only: no signer page (it is where private keys are typed; nothing on this site signs)
      input: { main: fileURLToPath(new URL('./index.html', import.meta.url)), ...(landingOnly ? {} : { signer: fileURLToPath(new URL('./signer.html', import.meta.url)) }) },
      output: { keepNames: true },
    },
  },
  optimizeDeps: { exclude: ['kob-wasm'] },
  server: { port: 5173, strictPort: false },
  preview: { port: 4173 },
});
