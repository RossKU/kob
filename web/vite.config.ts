import { defineConfig } from 'vite';
import preact from '@preact/preset-vite';
import { fileURLToPath } from 'node:url';
import { CSP_META } from './csp.mjs';
import { defaultRegistrySha256 } from './registry-pin.mjs';

// Embeds the Content-Security-Policy (csp.mjs) into the BUILT index.html only: the dev server needs inline scripts for hot reload.
const kobCsp = () => ({
  name: 'kob-csp',
  apply: 'build' as const,
  transformIndexHtml: () => [{ tag: 'meta', attrs: { 'http-equiv': 'Content-Security-Policy', content: CSP_META }, injectTo: 'head-prepend' as const }],
});

// Static, client-side build: `vite build` -> dist/ (serve from any static host; relative asset URLs).
export default defineConfig({
  base: './',
  // sha256 of registry/tokens.json at build time: the pin the loaded registry is compared with (src/kob/registry-source.ts)
  define: { __KOB_DEFAULT_REGISTRY_SHA256__: JSON.stringify(defaultRegistrySha256()) },
  plugins: [preact(), kobCsp()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  // keepNames: the official kaspa-wasm SDK identifies its JS classes (Transaction, TransactionOutput, Hash ...) by their `name` when it converts
  // request objects (rpc.submitTransaction: "Error converting property `covenant`" once a minifier renamed them). Found by the real-wallet TN10 run.
  build: { target: 'es2022', sourcemap: true, chunkSizeWarningLimit: 4000, rolldownOptions: { output: { keepNames: true } } },
  optimizeDeps: { exclude: ['kob-wasm'] },
  server: { port: 5173, strictPort: false },
  preview: { port: 4173 },
});
