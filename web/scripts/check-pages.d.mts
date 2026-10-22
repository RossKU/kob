/** Every file under `dir`, as URL paths (`/assets/x.js`), sorted. */
export function distFiles(dir: string): string[];
/** The post-deploy checks of a hosted web app (headers of `/`, the signer page and `/assets/`; with `dist`, every built file's bytes). */
export function checkPages(
  base: string,
  o?: { dist?: string; fetchFn?: typeof fetch; attempts?: number; delayMs?: number },
): Promise<{ ok: boolean; lines: string[] }>;
