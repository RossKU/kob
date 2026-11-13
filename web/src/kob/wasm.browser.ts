// Browser loader of kob-wasm. It uses the wasm-bindgen `--target web` bindings that
// `npm run build:wasm` copies to web/wasm/web.
import { createKob, type KobWasm, type RawKobWasm } from './wasm';

let cached: Promise<KobWasm> | null = null;

/**
 * Loads and initialises kob-wasm once, and verifies the embedded templates with `selfCheck` before
 * returning.
 */
export function loadKob(): Promise<KobWasm> {
  cached ??= (async () => {
    const mod = (await import('../../wasm/web/kob_wasm.js')) as unknown as { default: (input?: unknown) => Promise<unknown> } & RawKobWasm;
    const wasmUrl = (await import('../../wasm/web/kob_wasm_bg.wasm?url')).default as string;
    await mod.default({ module_or_path: wasmUrl });
    const kob = createKob(mod);
    kob.selfCheck();
    return kob;
  })();
  return cached;
}
