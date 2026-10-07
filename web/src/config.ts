// Application configuration. Layers, later wins:
//   1. defaults (mainnet, no indexer, SDK public Resolver node, bundled registry, Kastle off)
//   2. ./config.json          (optional, fetched next to the app; failure ignored)
//   3. window.__KOB_CONFIG__  (injected by tests / the hosting page)
//   4. localStorage `kob.settings` (the Settings screen)
//   5. URL query `?indexer=&node=&network=&kastle=1`  ONLY when the merged config so far has `allowQueryOverrides: true`
//      (default false: a link must not silently repoint the app at another indexer or node).
// `features.test` can only come from layer 3: it exposes the SDK to the test-only mock wallets and must never be switchable by a link,
// a file next to the app, or a stored setting.
//
// `mergeConfig` is pure; `loadConfig` reads the environment through injectable dependencies (no DOM access in the pure part).
import { normalizeNetwork } from './data/kaspa-sdk';
import { DEFAULT_FEES, FEE_MAX_KAS_MAX, FEE_MAX_RATE_MAX, FEE_MAX_RATE_MIN, parseMaxFeeKas, parseMaxRate, type FeeSettings } from './kob/fee-policy';

export type NetworkName = 'mainnet' | 'testnet-10';

export interface AppConfig {
  network: NetworkName;
  /** REST + WebSocket base of the indexer, e.g. `https://kob.example/`; '' = no indexer (orders/book unavailable) */
  indexerUrl: string;
  /**
   * further indexers the key facts of a token (standing, powers, best prices) are cross-checked against (config.json / `__KOB_CONFIG__`, or a
   * stored setting, which every page then names in a banner; a link must not add verifiers). Empty = a single indexer, unverified.
   */
  extraIndexerUrls: string[];
  /** wRPC (`ws://` / `wss://`) node, '' = the SDK's public Resolver; a mock-server base (`http://` / `https://`) only in test / dev builds */
  nodeUrl: string;
  /** token registry JSON: a relative path (bundled) or an http(s) URL */
  registryUrl: string;
  /** `priorityFee`: pay the storage-inclusive fee instead of the node's relay floor (default off) */
  features: { kastle: boolean; test: boolean; priorityFee: boolean };
  /**
   * Dynamic priority fee (kob/fee-policy.ts): the rate each transaction pays comes from the node's fee estimate and the action's urgency instead of
   * the relay floor. `dynamic: false` = always the floor; `maxRate` = highest rate in sompi per gram (floor 100); `maxFeeKas` = most one
   * transaction pays in total, KAS (0 = no total cap).
   */
  fees: FeeSettings;
  /** honour `?indexer=&node=...` (default false) */
  allowQueryOverrides: boolean;
  /**
   * Tokens that stand for a fiat currency (`{ "<covenant id>": "USD" }`): the first registry token marked `USD` is the app's USD reference (e.g. the TN10
   * soak's TUSD, which tracks 1 USD worth of KAS). It only sets the landing screen (that token's market opens as TUSD/KAS like every other
   * market); no ticker is ever renamed and there are no USD-only views (old `#/usd/...` links redirect to the token market / the pair page).
   * config.json / `__KOB_CONFIG__` only: a link or a stored setting must not mark a token as a dollar. Empty = every market TOKEN/KAS.
   */
  quoteTokens: Record<string, QuoteCurrency>;
  /**
   * The landing screen (empty hash): `auto` (default: the USD token's market, <ticker>/KAS, when a USD reference token is configured, else the first
   * tradable registry token's market, else the market list), `list`, `kas-usd` (the USD token's market), `usd:<covenant id>` (the pair page `<id>/<USD token>`) or `market:<covenant id>`
   * (that token's market page). The market list stays at `#/market` (header navigation).
   */
  home: HomeSetting;
  /**
   * Block explorer base URL for transaction links (recent trades, My orders): `https://tn10.kaspa.stream` on testnet-10, `https://kaspa.stream` on mainnet
   * (see `explorerTxUrl`). '' = that default for `network`. config.json / `__KOB_CONFIG__` only: a link or a stored setting must not repoint the links.
   */
  explorerUrl: string;
  /**
   * How far (basis points) the start of a market / close order (the best price of the indexer book) may be on the costly side of an independent
   * reference (the last fill, another indexer's best price) before the ticket holds the order until the user acknowledges it. config.json /
   * `__KOB_CONFIG__` only (a link or a stored setting must not loosen it); 10 to 5000, default 1000 (10 %).
   */
  marketStartToleranceBps: number;
}

export const MARKET_START_TOLERANCE_MIN = 10;
export const MARKET_START_TOLERANCE_MAX = 5_000;

export type QuoteCurrency = 'USD';
export type HomeSetting = 'auto' | 'list' | 'kas-usd' | `usd:${string}` | `market:${string}`;

export { DEFAULT_FEES, FEE_MAX_KAS_MAX, FEE_MAX_RATE_MAX, FEE_MAX_RATE_MIN, parseMaxFeeKas, parseMaxRate, type FeeSettings };

export const SETTINGS_KEY = 'kob.settings';
export const CONFIG_JSON_PATH = './config.json';

export const DEFAULT_CONFIG: Readonly<AppConfig> = Object.freeze({
  network: 'mainnet' as NetworkName,
  indexerUrl: '',
  extraIndexerUrls: Object.freeze([]) as unknown as string[],
  nodeUrl: '',
  registryUrl: './registry/tokens.json',
  features: Object.freeze({ kastle: false, test: false, priorityFee: false }) as { kastle: boolean; test: boolean; priorityFee: boolean },
  fees: DEFAULT_FEES,
  allowQueryOverrides: false,
  quoteTokens: Object.freeze({}) as Record<string, QuoteCurrency>,
  home: 'auto' as HomeSetting,
  explorerUrl: '',
  marketStartToleranceBps: 1_000,
});

/** Default block explorer of each network (kaspa.stream: `<base>/transactions/<txid>`). */
export const DEFAULT_EXPLORERS: Readonly<Record<NetworkName, string>> = Object.freeze({ mainnet: 'https://kaspa.stream', 'testnet-10': 'https://tn10.kaspa.stream' });

/** The explorer base URL in force: the configured one, else the default of the network. */
export const explorerBase = (c: Pick<AppConfig, 'network' | 'explorerUrl'>): string => c.explorerUrl || DEFAULT_EXPLORERS[c.network];

/** Explorer page of a transaction, or null for anything that is not a 64-hex transaction id. */
export function explorerTxUrl(c: Pick<AppConfig, 'network' | 'explorerUrl'>, txid: string | null | undefined): string | null {
  if (typeof txid !== 'string' || !/^[0-9a-f]{64}$/i.test(txid)) return null;
  return `${explorerBase(c)}/transactions/${txid.toLowerCase()}`;
}

const HOME_RE = /^(auto|list|kas-usd|(usd|market):[0-9a-f]{64})$/;
/** A valid landing-screen setting, or null. */
export function parseHome(v: unknown): HomeSetting | null {
  if (typeof v !== 'string') return null;
  const s = v.trim().toLowerCase();
  return HOME_RE.test(s) ? (s as HomeSetting) : null;
}

const MAX_EXTRA_INDEXERS = 4;
const RPC_SCHEMES = ['ws:', 'wss:', 'http:', 'https:'];
const HTTP_SCHEMES = ['http:', 'https:'];

/** Trimmed absolute URL of an allowed scheme, without a trailing slash; null when invalid. '' is valid (= unset). */
export function validateUrl(value: unknown, schemes: readonly string[] = RPC_SCHEMES): string | null {
  if (typeof value !== 'string') return null;
  const v = value.trim();
  if (v === '') return '';
  let u: URL;
  try {
    u = new URL(v);
  } catch {
    return null;
  }
  if (!schemes.includes(u.protocol) || !u.hostname) return null;
  if (u.username || u.password) return null; // credentials in a URL end up in logs and referrers
  return v.replace(/\/+$/, '');
}

/** Registry location: a relative path (`./registry/tokens.json`, `/x.json`) or an http(s) URL. */
export function validateRegistryUrl(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  const v = value.trim();
  if (v === '') return null;
  if (/^[a-z][a-z0-9+.-]*:/i.test(v)) return validateUrl(v, HTTP_SCHEMES) || null;
  if (v.startsWith('//') || /\s/.test(v)) return null;
  return v;
}

export function parseNetwork(value: unknown): NetworkName | null {
  const n = normalizeNetwork(value);
  return n === 'mainnet' || n === 'testnet-10' ? n : null;
}


/** `1` / `true` / `on` / `yes` -> true, `0` / `false` / `off` / `no` -> false, anything else -> null */
function parseFlag(v: unknown): boolean | null {
  if (typeof v === 'boolean') return v;
  if (typeof v !== 'string') return null;
  const s = v.trim().toLowerCase();
  if (['1', 'true', 'on', 'yes'].includes(s)) return true;
  if (['0', 'false', 'off', 'no'].includes(s)) return false;
  return null;
}

/** One config layer after validation; only fields that were present AND valid are set. */
export interface ConfigPatch {
  network?: NetworkName;
  indexerUrl?: string;
  extraIndexerUrls?: string[];
  nodeUrl?: string;
  registryUrl?: string;
  kastle?: boolean;
  test?: boolean;
  priorityFee?: boolean;
  feesDynamic?: boolean;
  feesMaxRate?: number;
  feesMaxFeeKas?: number;
  allowQueryOverrides?: boolean;
  quoteTokens?: Record<string, QuoteCurrency>;
  home?: HomeSetting;
  explorerUrl?: string;
  marketStartToleranceBps?: number;
}

export interface ConfigWarning { layer: string; field: string; message: string }

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** Validates a JSON-ish layer into a patch. `allowTest` is true only for the injected layer. */
export function sanitizeLayer(layer: string, raw: unknown, allowTest: boolean, warnings: ConfigWarning[] = []): ConfigPatch {
  const out: ConfigPatch = {};
  if (raw === undefined || raw === null) return out;
  if (!isObj(raw)) {
    warnings.push({ layer, field: '*', message: 'not an object, ignored' });
    return out;
  }
  const bad = (field: string, what: string) => warnings.push({ layer, field, message: `invalid ${what}, ignored` });
  if ('network' in raw) {
    const n = parseNetwork(raw.network);
    if (n) out.network = n;
    else bad('network', 'network (use mainnet or testnet-10)');
  }
  for (const f of ['indexerUrl', 'nodeUrl'] as const) {
    if (f in raw) {
      const u = validateUrl(raw[f]);
      if (u !== null) out[f] = u;
      else bad(f, 'URL (use ws, wss, http or https)');
    }
  }
  if ('extraIndexerUrls' in raw) {
    if (Array.isArray(raw.extraIndexerUrls)) {
      const urls = raw.extraIndexerUrls.map((u) => validateUrl(u)).filter((u): u is string => !!u);
      if (urls.length === raw.extraIndexerUrls.length && urls.length <= MAX_EXTRA_INDEXERS) out.extraIndexerUrls = [...new Set(urls)];
      else bad('extraIndexerUrls', `list of at most ${MAX_EXTRA_INDEXERS} URLs (use http or https)`);
    } else bad('extraIndexerUrls', 'list of URLs');
  }
  if ('registryUrl' in raw) {
    const u = validateRegistryUrl(raw.registryUrl);
    if (u !== null) out.registryUrl = u;
    else bad('registryUrl', 'registry URL');
  }
  if ('allowQueryOverrides' in raw) {
    const f = parseFlag(raw.allowQueryOverrides);
    if (f !== null) out.allowQueryOverrides = f;
    else bad('allowQueryOverrides', 'flag');
  }
  if ('features' in raw) {
    if (isObj(raw.features)) {
      if ('kastle' in raw.features) {
        const f = parseFlag(raw.features.kastle);
        if (f !== null) out.kastle = f;
        else bad('features.kastle', 'flag');
      }
      if ('priorityFee' in raw.features) {
        const f = parseFlag(raw.features.priorityFee);
        if (f !== null) out.priorityFee = f;
        else bad('features.priorityFee', 'flag');
      }
      if ('test' in raw.features) {
        if (!allowTest) warnings.push({ layer, field: 'features.test', message: 'only honoured in window.__KOB_CONFIG__, ignored' });
        else {
          const f = parseFlag(raw.features.test);
          if (f !== null) out.test = f;
          else bad('features.test', 'flag');
        }
      }
    } else bad('features', 'features object');
  }
  // fiat labels: only from the deployment (config.json / __KOB_CONFIG__), never from a link or a stored setting
  const deployment = layer === 'config.json' || layer === '__KOB_CONFIG__';
  if ('quoteTokens' in raw) {
    if (!deployment) warnings.push({ layer, field: 'quoteTokens', message: 'only honoured in config.json / window.__KOB_CONFIG__, ignored' });
    else if (isObj(raw.quoteTokens) && Object.entries(raw.quoteTokens).every(([k, v]) => /^[0-9a-f]{64}$/i.test(k) && v === 'USD')) {
      out.quoteTokens = Object.fromEntries(Object.keys(raw.quoteTokens).map((k) => [k.toLowerCase(), 'USD' as QuoteCurrency]));
    } else bad('quoteTokens', 'map of covenant id -> "USD"');
  }
  if ('explorerUrl' in raw) {
    if (!deployment) warnings.push({ layer, field: 'explorerUrl', message: 'only honoured in config.json / window.__KOB_CONFIG__, ignored' });
    else {
      const u = validateUrl(raw.explorerUrl, HTTP_SCHEMES);
      if (u !== null) out.explorerUrl = u;
      else bad('explorerUrl', 'URL (use http or https)');
    }
  }
  if ('marketStartToleranceBps' in raw) {
    const v = raw.marketStartToleranceBps;
    if (!deployment) warnings.push({ layer, field: 'marketStartToleranceBps', message: 'only honoured in config.json / window.__KOB_CONFIG__, ignored' });
    else if (typeof v === 'number' && Number.isInteger(v) && v >= MARKET_START_TOLERANCE_MIN && v <= MARKET_START_TOLERANCE_MAX) out.marketStartToleranceBps = v;
    else bad('marketStartToleranceBps', `basis points (a whole number, ${MARKET_START_TOLERANCE_MIN} to ${MARKET_START_TOLERANCE_MAX})`);
  }
  if ('home' in raw) {
    const h = parseHome(raw.home);
    if (h) out.home = h;
    else bad('home', 'landing screen (auto, list, kas-usd, usd:<covenant id>, market:<covenant id>)');
  }
  if ('fees' in raw) {
    if (isObj(raw.fees)) {
      if ('dynamic' in raw.fees) {
        const f = parseFlag(raw.fees.dynamic);
        if (f !== null) out.feesDynamic = f;
        else bad('fees.dynamic', 'flag');
      }
      if ('maxRate' in raw.fees) {
        const r = parseMaxRate(raw.fees.maxRate);
        if (r !== null) out.feesMaxRate = r;
        else bad('fees.maxRate', `rate (a whole number of sompi per gram, ${FEE_MAX_RATE_MIN} to ${FEE_MAX_RATE_MAX})`);
      }
      if ('maxFeeKas' in raw.fees) {
        const k = parseMaxFeeKas(raw.fees.maxFeeKas);
        if (k !== null) out.feesMaxFeeKas = k;
        else bad('fees.maxFeeKas', `amount (KAS, 0 to ${FEE_MAX_KAS_MAX}, at most 8 decimals; 0 = no cap)`);
      }
    } else bad('fees', 'fees object');
  }
  return out;
}

/** Query-string layer: `?indexer=&node=&network=&kastle=1` (never `test`). */
export function queryLayer(search: string | URLSearchParams, warnings: ConfigWarning[] = []): ConfigPatch {
  const q = typeof search === 'string' ? new URLSearchParams(search) : search;
  const raw: Record<string, unknown> = {};
  if (q.has('indexer')) raw.indexerUrl = q.get('indexer');
  if (q.has('node')) raw.nodeUrl = q.get('node');
  if (q.has('network')) raw.network = q.get('network');
  if (q.has('kastle')) raw.features = { kastle: q.get('kastle') };
  const patch = sanitizeLayer('query', raw, false, warnings);
  delete patch.allowQueryOverrides; // a link must not unlock further overrides
  return patch;
}

export interface ConfigLayers {
  /** parsed `./config.json` */
  file?: unknown;
  /** `window.__KOB_CONFIG__` */
  injected?: unknown;
  /** parsed `kob.settings` */
  settings?: unknown;
  /** `location.search` (string with or without `?`) or URLSearchParams */
  query?: string | URLSearchParams;
}

/**
 * A node or indexer URL that this browser's stored settings (localStorage `kob.settings`) put in force instead of the deployment's: it applies to every
 * later visit, so the app says so on every page (`deployment`: what config.json / `__KOB_CONFIG__` / the defaults would have used).
 */
export interface StoredOverride {
  /**
   * `extraIndexerUrls`: the stored list of further indexers (`value` / `deployment`: the URLs joined by ', ') that the market start is checked
   * against; `allowQueryOverrides`: a stored `true` that lets links repoint the node and the indexer (`value` 'true', `deployment` 'false').
   */
  field: 'indexerUrl' | 'nodeUrl' | 'extraIndexerUrls' | 'allowQueryOverrides';
  value: string;
  deployment: string;
}

export interface ResolvedConfig { config: AppConfig; warnings: ConfigWarning[]; storedOverrides: StoredOverride[] }

function apply(base: AppConfig, p: ConfigPatch): AppConfig {
  return {
    network: p.network ?? base.network,
    indexerUrl: p.indexerUrl ?? base.indexerUrl,
    extraIndexerUrls: p.extraIndexerUrls ?? base.extraIndexerUrls,
    nodeUrl: p.nodeUrl ?? base.nodeUrl,
    registryUrl: p.registryUrl ?? base.registryUrl,
    features: { kastle: p.kastle ?? base.features.kastle, test: p.test ?? base.features.test, priorityFee: p.priorityFee ?? base.features.priorityFee },
    fees: { dynamic: p.feesDynamic ?? base.fees.dynamic, maxRate: p.feesMaxRate ?? base.fees.maxRate, maxFeeKas: p.feesMaxFeeKas ?? base.fees.maxFeeKas },
    allowQueryOverrides: p.allowQueryOverrides ?? base.allowQueryOverrides,
    quoteTokens: p.quoteTokens ?? base.quoteTokens,
    home: p.home ?? base.home,
    explorerUrl: p.explorerUrl ?? base.explorerUrl,
    marketStartToleranceBps: p.marketStartToleranceBps ?? base.marketStartToleranceBps,
  };
}

/** Pure merge of all layers; invalid values are ignored and reported in `warnings`. */
export function resolveConfig(layers: ConfigLayers = {}): ResolvedConfig {
  const warnings: ConfigWarning[] = [];
  let cfg = apply({ ...DEFAULT_CONFIG, features: { ...DEFAULT_CONFIG.features }, fees: { ...DEFAULT_CONFIG.fees }, quoteTokens: { ...DEFAULT_CONFIG.quoteTokens } }, {});
  cfg = apply(cfg, sanitizeLayer('config.json', layers.file, false, warnings));
  cfg = apply(cfg, sanitizeLayer('__KOB_CONFIG__', layers.injected, true, warnings));
  const deployment = cfg;
  const stored = sanitizeLayer('settings', layers.settings, false, warnings);
  cfg = apply(cfg, stored);
  if (layers.query !== undefined && layers.query !== '') {
    if (cfg.allowQueryOverrides) cfg = apply(cfg, queryLayer(layers.query, warnings));
    else {
      const q = typeof layers.query === 'string' ? new URLSearchParams(layers.query) : layers.query;
      const used = ['indexer', 'node', 'network', 'kastle'].filter((k) => q.has(k));
      if (used.length) warnings.push({ layer: 'query', field: used.join(','), message: 'query overrides are disabled (allowQueryOverrides is false), ignored' });
    }
  }
  // only a stored value that is still in force (a link did not replace it) and differs from the deployment's
  const storedOverrides: StoredOverride[] = (['indexerUrl', 'nodeUrl'] as const)
    .filter((f) => stored[f] !== undefined && stored[f] === cfg[f] && cfg[f] !== deployment[f])
    .map((f): StoredOverride => ({ field: f, value: cfg[f], deployment: deployment[f] }));
  // the further indexers a stored setting names are the references of the market-start check: said like a stored indexer
  const list = (xs: readonly string[]): string => xs.join(', ');
  if (stored.extraIndexerUrls !== undefined && list(cfg.extraIndexerUrls) !== list(deployment.extraIndexerUrls)) {
    storedOverrides.push({ field: 'extraIndexerUrls', value: list(cfg.extraIndexerUrls), deployment: list(deployment.extraIndexerUrls) });
  }
  if (stored.allowQueryOverrides === true && !deployment.allowQueryOverrides) {
    storedOverrides.push({ field: 'allowQueryOverrides', value: 'true', deployment: 'false' });
  }
  return { config: cfg, warnings, storedOverrides };
}

export const mergeConfig = (layers: ConfigLayers = {}): AppConfig => resolveConfig(layers).config;

// ------------------------------------------------------------------------------------------------ user settings (localStorage)

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

/** The browser's localStorage, or null when unavailable or blocked (the accessor itself can throw). */
export function defaultStorage(): StorageLike | null {
  try {
    return typeof localStorage !== 'undefined' ? localStorage : null;
  } catch {
    return null;
  }
}

export function readSettings(storage: StorageLike | null = defaultStorage()): unknown {
  try {
    const raw = storage?.getItem(SETTINGS_KEY);
    return raw ? JSON.parse(raw) : undefined;
  } catch {
    return undefined;
  }
}

/** What the Settings screen can persist. Returns false when storage is unavailable. */
export interface UserSettings {
  network?: NetworkName;
  indexerUrl?: string;
  nodeUrl?: string;
  registryUrl?: string;
  features?: { kastle?: boolean; priorityFee?: boolean };
  fees?: { dynamic?: boolean; maxRate?: number; maxFeeKas?: number };
  allowQueryOverrides?: boolean;
}

/** Merges `patch` into the stored settings (validated: invalid fields are dropped). `null` values remove a field. */
export function saveSettings(patch: Record<string, unknown>, storage: StorageLike | null = defaultStorage()): boolean {
  try {
    if (!storage) return false;
    const current = readSettings(storage);
    const merged: Record<string, unknown> = isObj(current) ? { ...current } : {};
    for (const [k, v] of Object.entries(patch)) {
      if (v === null) delete merged[k];
      else if (k === 'features' && isObj(v)) merged.features = { ...(isObj(merged.features) ? merged.features : {}), ...v };
      else if (k === 'fees' && isObj(v)) merged.fees = { ...(isObj(merged.fees) ? merged.fees : {}), ...v };
      else merged[k] = v;
    }
    // never persist `features.test`, even if a caller passes it
    if (isObj(merged.features)) delete merged.features.test;
    const clean = sanitizeLayer('settings', merged, false);
    const out: UserSettings = {};
    if (clean.network) out.network = clean.network;
    if (clean.indexerUrl !== undefined) out.indexerUrl = clean.indexerUrl;
    if (clean.nodeUrl !== undefined) out.nodeUrl = clean.nodeUrl;
    if (clean.registryUrl !== undefined) out.registryUrl = clean.registryUrl;
    if (clean.kastle !== undefined || clean.priorityFee !== undefined) {
      out.features = { ...(clean.kastle !== undefined ? { kastle: clean.kastle } : {}), ...(clean.priorityFee !== undefined ? { priorityFee: clean.priorityFee } : {}) };
    }
    if (clean.feesDynamic !== undefined || clean.feesMaxRate !== undefined || clean.feesMaxFeeKas !== undefined) {
      out.fees = {
        ...(clean.feesDynamic !== undefined ? { dynamic: clean.feesDynamic } : {}),
        ...(clean.feesMaxRate !== undefined ? { maxRate: clean.feesMaxRate } : {}),
        ...(clean.feesMaxFeeKas !== undefined ? { maxFeeKas: clean.feesMaxFeeKas } : {}),
      };
    }
    if (clean.allowQueryOverrides !== undefined) out.allowQueryOverrides = clean.allowQueryOverrides;
    storage.setItem(SETTINGS_KEY, JSON.stringify(out));
    return true;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------------------------------------ loadConfig

export interface LoadConfigDeps {
  fetch?: typeof fetch | null;
  /** `window.__KOB_CONFIG__` value (default: read from `globalThis`) */
  injected?: unknown;
  storage?: StorageLike | null;
  /** `location.search` (default: `globalThis.location`) */
  search?: string;
  configUrl?: string;
  /** fetch timeout for config.json, ms */
  timeoutMs?: number;
}

async function fetchConfigJson(deps: LoadConfigDeps): Promise<unknown> {
  const f = deps.fetch === undefined ? (typeof fetch === 'function' ? fetch : null) : deps.fetch;
  if (!f) return undefined;
  const ctl = typeof AbortController !== 'undefined' ? new AbortController() : null;
  const timer = ctl ? setTimeout(() => ctl.abort(), deps.timeoutMs ?? 4000) : null;
  try {
    const res = await f(deps.configUrl ?? CONFIG_JSON_PATH, { cache: 'no-cache', signal: ctl?.signal });
    if (!res.ok) return undefined;
    return await res.json();
  } catch {
    return undefined; // optional file: a missing or malformed config.json must never stop the app
  } finally {
    if (timer) clearTimeout(timer);
  }
}

/** Reads every layer from the environment (all injectable) and merges them; also reports what was ignored. */
export async function loadConfigWithWarnings(deps: LoadConfigDeps = {}): Promise<ResolvedConfig> {
  const g = globalThis as { __KOB_CONFIG__?: unknown; location?: { search?: string } };
  const layers: ConfigLayers = {
    file: await fetchConfigJson(deps),
    injected: 'injected' in deps ? deps.injected : g.__KOB_CONFIG__,
    settings: readSettings(deps.storage === undefined ? defaultStorage() : deps.storage),
    query: deps.search !== undefined ? deps.search : g.location?.search ?? '',
  };
  return resolveConfig(layers);
}

/** The merged application configuration (invalid values are silently ignored: see loadConfigWithWarnings). */
export const loadConfig = async (deps: LoadConfigDeps = {}): Promise<AppConfig> => (await loadConfigWithWarnings(deps)).config;
