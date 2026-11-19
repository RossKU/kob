// Bundles the soak into one node ESM file, dist/soak.mjs.
// The entry is src/main.ts. It imports the web app's planners from web/src and the x402 SDK from
// packages/kob-x402.
// Everything is inlined, so no node_modules is needed at run time.
// The two wasm modules (kob-wasm node bindings, official kaspa SDK) are loaded at run time from the
// absolute paths in the config.
import { fileURLToPath } from 'node:url';

const web = fileURLToPath(new URL('../../web/src', import.meta.url));
// The web app's `import.meta.glob` (its i18n dictionary, web/src/i18n/index.ts) is expanded at
// bundle time, as vite does.
// Rolldown is the one in web/node_modules, because the soak has no node_modules of its own.
const { importGlobPlugin } = await import(new URL('../../web/node_modules/rolldown/dist/experimental-index.mjs', import.meta.url).href);
export default {
  input: { soak: 'src/main.ts' },
  platform: 'node',
  resolve: { alias: { '@': web } },
  plugins: [importGlobPlugin({ root: fileURLToPath(new URL('../..', import.meta.url)) })],
  // Browser-only dynamic imports of the web app (vite `?url` assets, the browser SDK and wasm
  // builds) are never reached in node.
  external: [/^node:/, /\?url$/, /vendor\/kaspa-web/, /wasm\/web\//],
  // one file: a rebuild while the bots run must never break a lazily imported chunk
  output: { dir: 'dist', format: 'esm', entryFileNames: '[name].mjs', keepNames: true, sourcemap: true, inlineDynamicImports: true },
};
