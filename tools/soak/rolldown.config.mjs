// Bundles the soak (src/main.ts, which imports the web app's planners from web/src and the x402 SDK from packages/kob-x402) into ONE
// node ESM file dist/soak.mjs. Everything is inlined (no node_modules needed at run time); the two wasm modules are loaded at run time
// from absolute paths given in the config (kob-wasm node bindings, official kaspa SDK).
import { fileURLToPath } from 'node:url';

const web = fileURLToPath(new URL('../../web/src', import.meta.url));
// the web app's `import.meta.glob` (its i18n dictionary, web/src/i18n/index.ts) expanded at bundle time as vite does; rolldown is the one in
// web/node_modules (the soak has no node_modules of its own)
const { importGlobPlugin } = await import(new URL('../../web/node_modules/rolldown/dist/experimental-index.mjs', import.meta.url).href);
export default {
  input: { soak: 'src/main.ts' },
  platform: 'node',
  resolve: { alias: { '@': web } },
  plugins: [importGlobPlugin({ root: fileURLToPath(new URL('../..', import.meta.url)) })],
  // browser-only dynamic imports of the web app (vite `?url` assets, the browser SDK / wasm builds) are never reached in node
  external: [/^node:/, /\?url$/, /vendor\/kaspa-web/, /wasm\/web\//],
  // one file: a rebuild while the bots run must never break a lazily imported chunk
  output: { dir: 'dist', format: 'esm', entryFileNames: '[name].mjs', keepNames: true, sourcemap: true, inlineDynamicImports: true },
};
