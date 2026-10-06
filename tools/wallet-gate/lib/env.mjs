// Tiny .env reader/writer (no dependency). .env is gitignored and holds secrets.
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { ROOT } from './node-kaspa.mjs';
export const ENV_PATH = join(ROOT, '.env');
export function loadEnv() {
  const out = {};
  if (existsSync(ENV_PATH)) {
    for (const line of readFileSync(ENV_PATH, 'utf8').split(/\r?\n/)) {
      const m = /^\s*([A-Za-z0-9_]+)\s*=\s*(.*?)\s*$/.exec(line);
      if (m && !line.trim().startsWith('#')) out[m[1]] = m[2].replace(/^["']|["']$/g, '');
    }
  }
  return { ...out, ...Object.fromEntries(Object.entries(process.env).filter(([k]) => k in out || /^(NODE_WS|DEV_PRIVATE_KEY|WALLET_ADDRESS|WALLET_PUBKEY|RECIPIENT_PUBKEY)$/.test(k))) };
}
export function upsertEnv(pairs) {
  let text = existsSync(ENV_PATH) ? readFileSync(ENV_PATH, 'utf8') : '';
  for (const [k, v] of Object.entries(pairs)) {
    const re = new RegExp(`^\s*${k}\s*=.*$`, 'm');
    text = re.test(text) ? text.replace(re, `${k}=${v}`) : text + (text && !text.endsWith('\n') ? '\n' : '') + `${k}=${v}\n`;
  }
  writeFileSync(ENV_PATH, text);
}
