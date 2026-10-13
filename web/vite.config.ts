import { defineConfig, type Plugin } from 'vite';
import preact from '@preact/preset-vite';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { CSP_META, SIGNER_CSP_HEADER, SIGNER_CSP_META } from './csp.mjs';

const isSignerPage = (path: string) => /(^|\/)signer\.html$/.test(path);

// Embeds the Content-Security-Policy (csp.mjs) into the BUILT pages only (index.html: the app's; signer.html: the private-key signer's own,
// stricter one): the dev server needs inline scripts for hot reload.
const kobCsp = () => ({
  name: 'kob-csp',
  apply: 'build' as const,
  transformIndexHtml: (_html: string, ctx: { path: string }) => [
    { tag: 'meta', attrs: { 'http-equiv': 'Content-Security-Policy', content: isSignerPage(ctx.path) ? SIGNER_CSP_META : CSP_META }, injectTo: 'head-prepend' as const },
  ],
});

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

// Static, client-side build: `vite build` -> dist/ (serve from any static host; relative asset URLs).
export default defineConfig({
  base: './',
  plugins: [preact(), kobCsp(), kobStableMapRefs(), kobSignerHeaders()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // keepNames: the official kaspa-wasm SDK identifies its JS classes (Transaction, TransactionOutput, Hash ...) by their `name` when it converts
  // request objects (rpc.submitTransaction: "Error converting property `covenant`" once a minifier renamed them). Found by the real-wallet TN10 run.
  // two pages: the app (index.html) and the private-key signer it frames (signer.html, src/signer)
  build: {
    target: 'es2022',
    sourcemap: true,
    chunkSizeWarningLimit: 4000,
    rolldownOptions: {
      input: { main: fileURLToPath(new URL('./index.html', import.meta.url)), signer: fileURLToPath(new URL('./signer.html', import.meta.url)) },
      output: { keepNames: true },
    },
  },
  optimizeDeps: { exclude: ['kob-wasm'] },
  server: { port: 5173, strictPort: false },
  preview: { port: 4173 },
});
