// A mocked kascov.io for the specs of the market list's token names and icons (src/data/kascov.ts). The real service is never reached: the fixtures
// answer every request to https://kascov.io/ with an empty token list, and `mockKascov` replaces that answer for one page. The routes answer like the
// real service does: tokens.json with CORS `*`, saved images at /img/<network>/<covenant id> with CORS `*` (404 where it holds no copy), and a creator's own
// host WITHOUT CORS (what storage.googleapis.com does), so the browser can show such a picture but never read its bytes.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import type { Page, Route } from '@playwright/test';

/** A 1x1 PNG, and a second, different one (a valid image whose hash is not the committed one). */
export const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==', 'base64');
export const OTHER_PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==', 'base64');
export const sha256 = (b: Buffer): string => createHash('sha256').update(b).digest('hex');

/** The template hash of a token program of the profile table (the same file the app is generated from). */
export function programHash(name: string): string {
  const doc = JSON.parse(readFileSync(new URL('../../../crates/kob-protocol/data/programs.json', import.meta.url), 'utf8')) as { programs: { name: string; template_hash: string }[] };
  const p = doc.programs.find((x) => x.name === name);
  if (!p) throw new Error(`no token program ${name}`);
  return p.template_hash;
}

export interface MockKascovToken {
  covenant_id: string;
  claimed_ticker?: string;
  claimed_name?: string;
  claimed_decimals?: number;
  claimed_image?: string;
  claimed_image_hash?: string;
  /** the program hash kascov names (`standard.view_template_hash`) */
  program?: string;
  status?: string;
  /**
   * kascov's `market`: a priced one carries the exact spot (`spot_num_sompi` over `spot_den`, sompi per base unit, numbers or digit strings as given), an unpriced one a reason.
   * No `market` at all is a token kascov has not priced either.
   */
  market?: { spot_num_sompi?: number | string; spot_den?: number | string } | { unpriced_reason: string };
}

/** One candle's close as kascov serves it: `quote_sompi` over `base_amount` at the bucket starting at `t` (ms). */
export interface MockKascovCandle { t: number; base: number | string; quote: number | string }

export interface MockKascovOptions {
  network?: string;
  /** where the service is (default https://kascov.io; the data set is at <origin>/data, the saved images at <origin>/img) */
  origin?: string;
  tokens: MockKascovToken[];
  /** saved images by covenant id; a token without one answers 404 */
  images?: Record<string, Buffer>;
  /** bodies of the creators' own hosts by URL (answered without CORS headers) */
  hosts?: Record<string, Buffer>;
  /** replaces the tokens.json answer: `down` aborts the request, `shape` answers a changed document, `http` a 503 */
  tokensMode?: 'ok' | 'down' | 'shape' | 'http';
  /** the per-token files `<data>/<network>/c/<covenant id>.json` (what KOB's own genesis check reads): a body, `'down'` or a status number; a token without one answers 404 */
  files?: Record<string, unknown>;
  /**
   * the candles of a token (`<data>/<network>/token/<id>/candles?bucket=..&phase=all`): a list, or a function of the bucket's length in ms; a token without one answers 404.
   * `candlesMode`: `http` answers a 503 instead, `down` aborts the request.
   */
  candles?: Record<string, MockKascovCandle[] | ((bucketMs: number) => MockKascovCandle[])>;
  candlesMode?: 'ok' | 'http' | 'down';
}

export interface KascovLog {
  /** every URL requested of kascov.io or a creator's host, in order */
  requests: string[];
  tokenRequests: number;
  /** the covenant ids whose per-token file was asked for, in order */
  fileRequests: string[];
  /** every candles URL requested, in order */
  candleRequests: string[];
}

const BUCKET_MS: Record<string, number> = { '1m': 60_000, '15m': 900_000, '1h': 3_600_000, '4h': 14_400_000, '1d': 86_400_000 };

const body = (o: MockKascovOptions) => ({
  network: o.network ?? 'testnet-10',
  generated_at_ms: 1_793_000_000_000,
  tokens: o.tokens.map((t) => ({
    covenant_id: t.covenant_id,
    ...(t.claimed_ticker !== undefined ? { claimed_ticker: t.claimed_ticker } : {}),
    ...(t.claimed_name !== undefined ? { claimed_name: t.claimed_name } : {}),
    ...(t.claimed_decimals !== undefined ? { claimed_decimals: t.claimed_decimals } : {}),
    ...(t.claimed_image !== undefined ? { claimed_image: t.claimed_image } : {}),
    ...(t.claimed_image_hash !== undefined ? { claimed_image_hash: t.claimed_image_hash } : {}),
    // kascov's remark: only `verified` shows the chip "Verified by kascov" (ui/genesis-check.spec.ts); the other specs are not about it
    ...(t.market !== undefined ? { market: t.market } : {}),
    status: t.status ?? 'unvalidated',
    alive: true,
    ...(t.program ? { standard: { kcc: 20, review: 'reviewed', template_hash: 'f'.repeat(64), view_template_hash: t.program } } : {}),
  })),
});

const CORS = { 'access-control-allow-origin': '*' };

/** Installs the mock on `page` (call before the first navigation, with `autoOpen: false`). */
export async function mockKascov(page: Page, o: MockKascovOptions): Promise<KascovLog> {
  const log: KascovLog = { requests: [], tokenRequests: 0, fileRequests: [], candleRequests: [] };
  const net = o.network ?? 'testnet-10';
  const origin = o.origin ?? 'https://kascov.io';
  await page.route(`${origin}/**`, (route: Route) => {
    const url = route.request().url();
    log.requests.push(url);
    if (url === `${origin}/data/${net}/tokens.json`) {
      log.tokenRequests++;
      switch (o.tokensMode ?? 'ok') {
        case 'down':
          return route.abort('failed');
        case 'http':
          return route.fulfill({ status: 503, headers: CORS, body: 'unavailable' });
        case 'shape':
          return route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify({ tokens: { not: 'a list' } }) });
        default:
          return route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify(body(o)) });
      }
    }
    // <data>/<network>/token/<id>/candles?bucket=<bucket>&phase=all
    const tokenPrefix = `${origin}/data/${net}/token/`;
    const cm = url.startsWith(tokenPrefix) ? /^([0-9a-f]{64})\/candles\?bucket=([0-9a-z]+)&phase=all$/.exec(url.slice(tokenPrefix.length)) : null;
    if (cm) {
      log.candleRequests.push(url);
      const id = cm[1]!;
      const bucket = cm[2]!;
      const list = o.candles?.[id];
      if (o.candlesMode === 'down') return route.abort('failed');
      if (o.candlesMode === 'http') return route.fulfill({ status: 503, headers: CORS, body: 'unavailable' });
      if (!list || !BUCKET_MS[bucket]) return route.fulfill({ status: 404, headers: CORS, body: 'not found' });
      const ms = BUCKET_MS[bucket]!;
      const candles = (typeof list === 'function' ? list(ms) : list).map((c) => ({ t: c.t, carried: false, trades: 1, close: { base_amount: c.base, quote_sompi: c.quote } }));
      return route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify({ bucket, bucket_ms: ms, network: net, token_id: id, candles }) });
    }
    const filePrefix = `${origin}/data/${net}/c/`;
    const fileId = url.startsWith(filePrefix) && url.endsWith('.json') ? url.slice(filePrefix.length, -5) : '';
    if (/^[0-9a-f]{64}$/.test(fileId)) {
      log.fileRequests.push(fileId);
      const f = o.files?.[fileId];
      if (f === 'down') return route.abort('failed');
      if (typeof f === 'number') return route.fulfill({ status: f, headers: CORS, body: '{}' });
      return f === undefined ? route.fulfill({ status: 404, headers: CORS, body: 'not found' }) : route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify(f) });
    }
    const prefix = `${origin}/img/${net}/`;
    const id = url.startsWith(prefix) ? url.slice(prefix.length) : '';
    const saved = /^[0-9a-f]{64}$/.test(id) ? o.images?.[id] : undefined;
    if (saved) return route.fulfill({ status: 200, contentType: 'image/png', headers: CORS, body: saved });
    return route.fulfill({ status: 404, headers: CORS, body: 'not found' });
  });
  for (const [url, bytes] of Object.entries(o.hosts ?? {})) {
    await page.route(url, (route: Route) => {
      log.requests.push(route.request().url());
      // no Access-Control-Allow-Origin, like the creators' hosts
      return route.fulfill({ status: 200, contentType: 'image/png', body: bytes });
    });
  }
  return log;
}

export interface ExplorerLog {
  /** every URL requested of the explorer, in order */
  requests: string[];
}

/**
 * A mocked Kaspa REST explorer (api.kaspa.org / api-tn10.kaspa.org, CORS `*`) for KOB's own genesis check: `txs` by transaction id (a token's genesis
 * transaction); anything else answers 404. The real explorers are never reached from a test.
 */
export async function mockExplorer(page: Page, o: { txs?: Record<string, unknown>; down?: boolean } = {}): Promise<ExplorerLog> {
  const log: ExplorerLog = { requests: [] };
  await page.route(/^https:\/\/api(-tn10)?\.kaspa\.org\//, (route: Route) => {
    const url = route.request().url();
    log.requests.push(url);
    if (o.down) return route.abort('failed');
    const m = /\/transactions\/([0-9a-f]{64})\?/.exec(url);
    const tx = m ? o.txs?.[m[1]!] : undefined;
    return tx ? route.fulfill({ status: 200, contentType: 'application/json', headers: CORS, body: JSON.stringify(tx) }) : route.fulfill({ status: 404, headers: CORS, body: '{}' });
  });
  return log;
}
