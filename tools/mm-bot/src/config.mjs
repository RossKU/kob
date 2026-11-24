// The JSON config merged over DEFAULTS. Relative paths are relative to the config file. README.md
// lists the fields.

import { existsSync, readFileSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';

export const DEFAULTS = {
  network: 'mainnet', // mainnet | testnet-10
  node: null, // wRPC URL of a node; null: the CLI's default (two public resolver nodes that must agree)
  indexer: null, // KOB indexer URL; null: the CLI's default for the network
  kobCli: 'kob', // path of the kob binary
  keyFile: null, // file holding the 64-hex key; null: the KOB_KEY environment variable
  dataDir: './run', // state, logs, the STOP file and the CLI's own state directory
  capitalUsd: 1000, // the capital base (100 %): ladder sizes, side caps and warnings are percentages of it
  token: {
    covenantId: null, // 64 hex: the token is pinned by covenant id, never by ticker
    decimals: null, // checked against what the CLI reports
    label: null, // a name for logs and messages only
    usdSource: 'kraken:USDCUSD', // what one token is worth in USD: 'kraken:<PAIR>' or 'fixed'
    usdFallback: null, // a rate to use when the live one is missing or stale; null: IDLE (quotes pulled) until it is live again, so the peg band is always checked (L25)
    pegUsd: 1.0, // the token's peg
    pegBand: 0.005, // IDLE when the live USD rate is further than this from the peg (a fraction)
  },
  retiredTokens: [], // covenant ids of earlier instances: their quotes are pulled and their UTXOs merged, nothing else
  reference: {
    pollSec: 30,
    windowMin: 5, // closed 1-minute candles per VWAP
    venues: ['bybit', 'kucoin', 'gate', 'kraken'],
    fallbackVenues: ['mexc'],
    fallbackBelow: 3, // add the fallback venues when fewer primary venues are fresh
    minVenues: 2,
    maxDeviation: 0.01, // drop a venue this far from the median (3 or more venues: one cannot move the median past the others)
    pairAgreement: 0.00125, // with only 2 venues they must agree this closely (below half the spread), else IDLE
    maxCandleAgeSec: 120, // a venue whose newest closed candle ended longer ago is stale
    timeoutMs: 10_000,
    usdtUsd: ['kraken', 'coinbase'], // sources of USDT/USD: every one is asked
    usdtAgreement: 0.002, // two USDT/USD readings further apart, or a lone one further from 1, give no rate (IDLE for the USDT venues); below half the spread
    usdtBand: 0.01, // a USDT/USD reading further than this from 1 is refused
    stableMaxAgeSec: 300, // a USDT/USD or token USD reading older than this is not used
    crossCheck: { enabled: true, everySec: 300, venues: ['mexc', 'kucoin', 'bybit'], warnDeviation: 0.01 },
  },
  quotes: {
    levels: 3, // per side
    spreadPct: 1.0, // the gap between the best bid and the best ask, % of the fair value: each sits spreadPct / 2 from it
    levelStepPct: 0.5, // each further level is this much further out, % of the fair value
    levelSizePct: 15, // each level's size, % of capitalUsd (converted to tokens with the token's USD rate)
    maxAskPct: 50, // the most the asks may hold in all, % of capitalUsd
    maxBidPct: 50, // the most the bids may hold in all, % of capitalUsd (converted to KAS with KAS/USD)
    kasReserve: 50, // KAS never put on bids: fees and the 1 KAS carrier of each order
    minOrderKas: 15, // smallest order worth quoting (an order needs a 10 KAS minimum fill)
    repriceThreshold: 0.0025, // amend when the fair value moved more than this since the order was quoted
    resizeBelow: 0.5, // cancel and place again an order left with less than this share of its level
    askCarrierMinKas: 0.2, // an ask's amend is paid from its carrier: below this it is cancelled and placed again
    mergeAboveUtxos: 1, // merge the free token UTXOs before placing an ask when there are more than this (the limit is 3 inputs)
    lifeMin: 90, // every quote is placed and amended with an expiry this far ahead (kob --gtd): conforming matchers stop filling a dead bot's quotes past it; they stay fillable until a refund ends them (soft)
    renewAfterMin: 45, // a quote not amended for this long is amended in place to a fresh expiry (a reprice renews it too)
    skew: { enabled: false, maxShift: 0.002, targetTokenShare: 0.5 },
    // true: asks take partial fills; false: every ask's minimum fill is its whole amount (filled in
    // full or not at all), and the node alone says which asks are live. null: not set, which
    // mainnet refuses (an informed choice) and testnet-10 takes as true.
    askPartialFills: null,
  },
  safety: {
    killJump: 0.02, // pull the quotes when the reference moved more than this within a minute
    resumeAfterSec: 180, // and quote again once it has been fresh and steady this long
    maxFeesKasPerDay: 50, // per UTC day
    cancelReserveKas: 5, // inside the cap: amends and placements stop this much before the cap, so cancels can still go
    placeGapSec: 8, // between placements, for the funding change to settle
    orderVisibleTimeoutSec: 180, // an action's result not seen by the indexer after this long is re-read
    maxActionsPerCycle: 12,
    backoffBaseSec: 15,
    backoffMaxSec: 300,
  },
  alerts: {
    warnBelowPct: 12.5, // warn when a side holds less than this, % of capitalUsd (a quarter of a 50 % side)
    remindMin: 60,
    webhookUrl: null, // optional: POST of JSON { level, message, asset, shortfall, address, ... }
  },
  cli: {
    timeoutSec: 180,
    preflight: true, // run every signing command with --dry-run first, then with --yes
    priorityFee: false,
  },
};

const isObj = (v) => v && typeof v === 'object' && !Array.isArray(v);

function merge(base, over) {
  if (!isObj(over)) return over === undefined ? base : over;
  const out = { ...base };
  for (const [k, v] of Object.entries(over)) out[k] = isObj(base?.[k]) && isObj(v) ? merge(base[k], v) : v;
  return out;
}

/** Reads, merges and checks a config file. Throws one error that lists every problem. */
export function loadConfig(path) {
  const raw = JSON.parse(readFileSync(path, 'utf8'));
  const cfg = merge(DEFAULTS, raw);
  const base = dirname(resolve(path));
  const abs = (p) => (p == null ? p : isAbsolute(p) ? p : resolve(base, p));
  cfg.dataDir = abs(cfg.dataDir);
  if (cfg.keyFile) cfg.keyFile = abs(cfg.keyFile);
  // A bare name (`kob`) is looked up on PATH, unless a file of that name sits next to the config.
  if (cfg.kobCli && (/[\\/]/.test(cfg.kobCli) || existsSync(abs(cfg.kobCli)))) cfg.kobCli = abs(cfg.kobCli);
  cfg.configPath = resolve(path);
  const problems = validate(cfg);
  if (problems.length) throw new Error(`config ${path}:\n  - ${problems.join('\n  - ')}`);
  return cfg;
}

/** Every problem of a merged config (empty when it is fine). */
export function validate(cfg) {
  const p = [];
  const num = (v, name, { min = -Infinity, max = Infinity, int = false } = {}) => {
    if (typeof v !== 'number' || !Number.isFinite(v) || v < min || v > max || (int && !Number.isInteger(v))) p.push(`${name} must be a${int ? 'n integer' : ' number'} in [${min}, ${max}], got ${JSON.stringify(v)}`);
  };
  if (!['mainnet', 'testnet-10'].includes(cfg.network)) p.push('network must be "mainnet" or "testnet-10"');
  if (cfg.node != null && !/^wss?:\/\//.test(cfg.node)) p.push('node must be a ws:// or wss:// URL, or null');
  if (cfg.indexer != null && !/^https?:\/\//.test(cfg.indexer)) p.push('indexer must be an http:// or https:// URL, or null');
  if (!cfg.kobCli) p.push('kobCli is required');
  if (cfg.keyFile != null && typeof cfg.keyFile !== 'string') p.push('keyFile must be a path or null');
  const t = cfg.token;
  if (!/^[0-9a-f]{64}$/.test(String(t.covenantId))) p.push('token.covenantId must be 64 lower-case hex (pin the token by its covenant id)');
  num(t.decimals, 'token.decimals', { min: 0, max: 18, int: true });
  if (!(t.usdSource === 'fixed' || /^kraken:[A-Z0-9]+$/.test(String(t.usdSource)))) p.push('token.usdSource must be "kraken:<PAIR>" (e.g. kraken:USDCUSD) or "fixed"');
  if (t.usdFallback != null) num(t.usdFallback, 'token.usdFallback', { min: 1e-9 });
  if (t.usdSource === 'fixed' && !(t.usdFallback > 0)) p.push('token.usdSource "fixed" needs token.usdFallback');
  num(t.pegUsd, 'token.pegUsd', { min: 1e-9 });
  num(t.pegBand, 'token.pegBand', { min: 0.0001, max: 0.5 });
  if (!Array.isArray(cfg.retiredTokens)) p.push('retiredTokens must be an array of covenant ids');
  else for (const id of cfg.retiredTokens) {
    if (!/^[0-9a-f]{64}$/.test(String(id))) p.push(`retiredTokens: ${JSON.stringify(id)} is not 64 lower-case hex`);
    if (id === t.covenantId) p.push('retiredTokens must not contain token.covenantId (the token the bot quotes)');
  }
  const r = cfg.reference;
  num(r.pollSec, 'reference.pollSec', { min: 10, max: 600 });
  num(r.windowMin, 'reference.windowMin', { min: 1, max: 60, int: true });
  num(r.minVenues, 'reference.minVenues', { min: 1, max: 10, int: true });
  num(r.maxDeviation, 'reference.maxDeviation', { min: 0.0001, max: 0.2 });
  if (r.pairAgreement != null) num(r.pairAgreement, 'reference.pairAgreement', { min: 0, max: 0.2 });
  num(r.usdtAgreement, 'reference.usdtAgreement', { min: 0, max: 0.05 });
  num(r.usdtBand, 'reference.usdtBand', { min: 0.001, max: 0.1 });
  num(r.maxCandleAgeSec, 'reference.maxCandleAgeSec', { min: 30, max: 3600 });
  num(r.timeoutMs, 'reference.timeoutMs', { min: 1000, max: 60_000 });
  const known = ['bybit', 'kucoin', 'gate', 'kraken', 'mexc'];
  for (const v of [...r.venues, ...r.fallbackVenues]) if (!known.includes(v)) p.push(`reference: unknown venue ${v} (known: ${known.join(', ')})`);
  for (const v of r.usdtUsd) if (!['kraken', 'coinbase'].includes(v)) p.push(`reference.usdtUsd: unknown source ${v}`);
  for (const v of r.crossCheck?.venues ?? []) if (!['mexc', 'kucoin', 'bybit'].includes(v)) p.push(`reference.crossCheck: unknown venue ${v}`);
  const q = cfg.quotes;
  num(q.levels, 'quotes.levels', { min: 1, max: 20, int: true });
  num(q.spreadPct, 'quotes.spreadPct', { min: 0.05, max: 50 });
  num(q.levelStepPct, 'quotes.levelStepPct', { min: 0, max: 50 });
  const half = q.spreadPct / 200;
  num(cfg.capitalUsd, 'capitalUsd', { min: 1 });
  num(q.levelSizePct, 'quotes.levelSizePct', { min: 0.01, max: 100 });
  num(q.maxAskPct, 'quotes.maxAskPct', { min: 0, max: 100 });
  num(q.maxBidPct, 'quotes.maxBidPct', { min: 0, max: 100 });
  if (q.maxAskPct + q.maxBidPct > 100) p.push('quotes.maxAskPct plus quotes.maxBidPct must not exceed 100 (% of capitalUsd)');
  num(q.kasReserve, 'quotes.kasReserve', { min: 0 });
  num(q.minOrderKas, 'quotes.minOrderKas', { min: 10 });
  num(q.repriceThreshold, 'quotes.repriceThreshold', { min: 0.0001, max: 0.5 });
  num(q.resizeBelow, 'quotes.resizeBelow', { min: 0, max: 1 });
  num(q.askCarrierMinKas, 'quotes.askCarrierMinKas', { min: 0.1 });
  if (q.askPartialFills != null && typeof q.askPartialFills !== 'boolean') p.push('quotes.askPartialFills must be true, false or null');
  num(q.mergeAboveUtxos, 'quotes.mergeAboveUtxos', { min: 1, max: 3, int: true });
  num(q.lifeMin, 'quotes.lifeMin', { min: 30, max: 1440 });
  num(q.renewAfterMin, 'quotes.renewAfterMin', { min: 5, max: 1440 });
  // The renewal must leave time for retries (backoff up to backoffMaxSec, an unseen amend re-read after
  // orderVisibleTimeoutSec) before the quote lapses.
  if (q.renewAfterMin > q.lifeMin - 15) p.push('quotes.renewAfterMin must be at least 15 minutes below quotes.lifeMin, so that a failed renewal is retried before the quote lapses');
  if (half + ((q.levels - 1) * q.levelStepPct) / 100 >= 1) p.push('quotes: the outermost bid would be at or below zero');
  if (r.pairAgreement == null ? r.minVenues < 3 : r.pairAgreement >= half) p.push('reference.pairAgreement must be below half of quotes.spreadPct (as a fraction), or two venues can move the fair value past the best quote');
  if (r.usdtAgreement >= half) p.push('reference.usdtAgreement must be below half of quotes.spreadPct (as a fraction), or one USDT/USD source can move the fair value past the best quote');
  // A wrong token USD rate inside the band moves fair by up to the band (R4T-6): at half the spread
  // the best quotes can reach the true fair value, never cross it.
  if (t.pegBand > half) p.push('token.pegBand must be at most half of quotes.spreadPct (as a fraction), or a wrong token USD rate inside the band can move the best quote past the fair value');
  if (q.repriceThreshold >= half) p.push('quotes.repriceThreshold must be below half of quotes.spreadPct (as a fraction), or a quote can drift through the fair value');
  if (q.skew?.enabled) {
    num(q.skew.maxShift, 'quotes.skew.maxShift', { min: 0, max: 0.5 });
    num(q.skew.targetTokenShare, 'quotes.skew.targetTokenShare', { min: 0, max: 1 });
    if (q.skew.maxShift + q.repriceThreshold >= half) p.push('quotes.skew.maxShift plus quotes.repriceThreshold must stay below half of quotes.spreadPct (no quote may cross the fair value)');
  }
  const s = cfg.safety;
  num(s.killJump, 'safety.killJump', { min: 0.001, max: 0.5 });
  num(s.resumeAfterSec, 'safety.resumeAfterSec', { min: 0, max: 86_400 });
  num(s.maxFeesKasPerDay, 'safety.maxFeesKasPerDay', { min: 0.01 });
  num(s.cancelReserveKas, 'safety.cancelReserveKas', { min: 0 });
  if (s.cancelReserveKas >= s.maxFeesKasPerDay) p.push('safety.cancelReserveKas must be below safety.maxFeesKasPerDay');
  num(s.placeGapSec, 'safety.placeGapSec', { min: 0, max: 600 });
  num(s.maxActionsPerCycle, 'safety.maxActionsPerCycle', { min: 1, max: 200, int: true });
  num(cfg.alerts.warnBelowPct, 'alerts.warnBelowPct', { min: 0, max: 100 });
  num(cfg.alerts.remindMin, 'alerts.remindMin', { min: 1 });
  if (cfg.alerts.webhookUrl != null && !/^https?:\/\//.test(cfg.alerts.webhookUrl)) p.push('alerts.webhookUrl must be an http(s) URL or null');
  num(cfg.cli.timeoutSec, 'cli.timeoutSec', { min: 10, max: 1800 });
  return p;
}
