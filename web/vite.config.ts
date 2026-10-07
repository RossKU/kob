import { defineConfig, type Plugin } from 'vite';
import preact from '@preact/preset-vite';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { CSP_META } from './csp.mjs';
import { defaultRegistrySha256 } from './registry-pin.mjs';

// Embeds the Content-Security-Policy (csp.mjs) into the BUILT index.html only: the dev server needs inline scripts for hot reload.
const kobCsp = () => ({
  name: 'kob-csp',
  apply: 'build' as const,
  transformIndexHtml: () => [{ tag: 'meta', attrs: { 'http-equiv': 'Content-Security-Policy', content: CSP_META }, injectTo: 'head-prepend' as const }],
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
  // sha256 of registry/tokens.json at build time: the pin the loaded registry is compared with (src/kob/registry-source.ts)
  define: { __KOB_DEFAULT_REGISTRY_SHA256__: JSON.stringify(defaultRegistrySha256()) },
  plugins: [preact(), kobCsp(), kobStableMapRefs()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // keepNames: the official kaspa-wasm SDK identifies its JS classes (Transaction, TransactionOutput, Hash ...) by their `name` when it converts
  // request objects (rpc.submitTransaction: "Error converting property `covenant`" once a minifier renamed them). Found by the real-wallet TN10 run.
  build: { target: 'es2022', sourcemap: true, chunkSizeWarningLimit: 4000, rolldownOptions: { output: { keepNames: true } } },
  optimizeDeps: { exclude: ['kob-wasm'] },
  server: { port: 5173, strictPort: false },
  preview: { port: 4173 },
});
