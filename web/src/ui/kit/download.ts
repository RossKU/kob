// Browser file helpers (download a JSON document, read a file the user picked). DOM only; the pure naming helpers are testable in node.

/** `kob-<what>-<network>-<yyyymmdd>.json` (UTC date): stable, sortable, no user text in the name. */
export function exportFileName(what: string, network: string, now: Date | number = new Date()): string {
  const d = new Date(now);
  const ymd = `${d.getUTCFullYear()}${String(d.getUTCMonth() + 1).padStart(2, '0')}${String(d.getUTCDate()).padStart(2, '0')}`;
  const safe = (s: string) => s.replace(/[^a-z0-9-]+/gi, '-').replace(/^-+|-+$/g, '').toLowerCase() || 'x';
  return `kob-${safe(what)}-${safe(network)}-${ymd}.json`;
}

/** JSON text of any value (bigint becomes a decimal string: amounts must never pass through `number`). */
export function jsonText(value: unknown): string {
  return JSON.stringify(value, (_k, v) => (typeof v === 'bigint' ? v.toString() : v), 2);
}

/** Triggers a browser download of `text` as `filename`. */
export function downloadText(filename: string, text: string, mime = 'application/json'): void {
  const url = URL.createObjectURL(new Blob([text], { type: mime }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.rel = 'noopener';
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** Reads a picked file as text (rejects files over `maxBytes`, default 5 MB: a backup is small, anything bigger is not one). */
export async function readFileText(file: File, maxBytes = 5_000_000): Promise<string> {
  if (file.size > maxBytes) throw new Error(`file too large (${file.size} bytes)`);
  return file.text();
}
