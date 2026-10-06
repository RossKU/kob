// Browser loader of kob-wasm (wasm-bindgen `--target web` bindings copied to web/wasm/web by `npm run build:wasm`).
import { createKob, type KobWasm, type RawKobWasm } from './wasm';

let cached: Promise<KobWasm> | null = null;

/** Loads and initialises kob-wasm once; verifies the embedded templates (`selfCheck`) before returning. */
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
