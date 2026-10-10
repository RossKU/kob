// Soak configuration (JSON). `config.example.json` is the committed template; the live copy is run/config.json.
import { readFileSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';
// relative (not `@/`): the fee settings are unit-tested by node --test, which has no path alias
import { DEFAULT_FEES, parseMaxFeeKas, parseMaxRate, type FeeSettings } from '../../../web/src/kob/fee-policy.ts';
import type { ConsolidateConfig } from './bots/consolidate-math';

export interface ExecutorConfig {
  /** process name, also the data directory under runDir */
  name: string;
  /** key name in run/keys.json (the operator hot key) */
  key: string;
  /** REST + WS listen address of the indexer API */
  api: string;
  /** x402 facilitator listen address (only one executor runs the facilitator) */
  x402?: string;
  /** extra `kob-executor run` flags */
  args?: string[];
}

export interface TraderConfig {
  /** key name in run/keys.json */
  key: string;
  /** mean seconds between actions (exponential inter-arrival) */
  meanIntervalSec: number;
  /** relative weights per action; missing = the default mix */
  weights?: Record<string, number>;
  /**
   * the largest order of this trader, in USD (= TUSD; on an asset book its value at the Binance reference). Sizes are drawn continuously in
   * token base units (any amount, never rounded to a step), mostly between half a dollar and this value.
   */
  maxUsd: number;
}

export interface TokenConfig {
  name: string;
  ticker: string;
  decimals: number;
  /** whole tokens */
  supply: string;
  /** minimum price step, sompi per whole token (the registry `tick`) */
  tick: string;
  /** share of the supply per holder key (rest to the bank) */
  allocation: Record<string, number>;
  /** issuance / registry description */
  description?: string;
}

/** A soak token beside TUSD (config `token2`, `token3`): its own KAS book, a USD reference from Binance, pair orders with TUSD. */
export interface Token2Config extends TokenConfig {
  /**
   * USD price of one whole token (Binance public REST returning `{price}`): the KAS reference of one whole token is usd / KASUSDT.
   * Only the bots read it.
   */
  priceUrl: string;
  /** share of the trader actions placed on this token's KAS book (the other non-pair actions trade `token`) */
  bookShare: number;
  /** share of the trader actions that are pair orders (every pair kind, `pairWeights`) of this token (A) against `token` (B) */
  pairShare?: number;
  /** older spelling of `pairShare` (the retired cross limits) */
  crossShare?: number;
  /** weights of the pair actions (default: `DEFAULT_PAIR_WEIGHTS` of bots/traders.ts) */
  pairWeights?: Record<string, number>;
  /** market-maker overrides for this book (default: the `mm` section) */
  mm?: Partial<SoakConfig['mm']>;
  /** size of one pair order: its USD value, drawn in this range (default [3, 20]) */
  pairUsd?: [number, number];
  /** older spelling of `pairUsd` */
  crossUsd?: [number, number];
  /** token fan-out per holder: outputs, and whole tokens per output (a decimal string), of the market maker and the traders */
  fanout?: { mm: [number, string]; traders: [number, string] };
}

export interface SoakConfig {
  network: string;
  nodeUrl: string;
  /** absolute or relative to the config file */
  runDir: string;
  /** kob-wasm node bindings directory (kob_wasm.js) */
  kobWasmDir: string;
  /** official kaspa SDK node directory (kaspa.js) */
  kaspaSdkDir: string;
  /** executor binary */
  executorBin: string;
  minerBin: string;
  /** CPU threads of the miner (older spelling of `miner.threads`) */
  minerThreads: number;
  /**
   * the miner child (validated and written to run/miner.json by scripts/miner-config.mjs; the miner re-reads that file on change):
   * backend, intermittent mining between `lowKas` and `highKas` of the bank's spendable KAS, duty cycle; absent = continuous
   */
  miner?: {
    backend?: 'cpu' | 'gpu' | 'auto';
    threads?: number;
    roundMs?: number;
    lowKas?: number;
    highKas?: number;
    maxDuty?: number;
    dutyWindowSec?: number;
    pollSec?: number;
    maturityDaa?: number;
    balanceAddress?: string;
    gpu?: { device?: string | number; global?: number; local?: number; dispatchMs?: number };
  };
  executors: ExecutorConfig[];
  /** indexer the bots and the checker read (one of the executors' APIs) */
  indexerUrl: string;
  /** a second indexer the checker compares against */
  indexerUrlB?: string;
  token: TokenConfig;
  /**
   * the second soak token (optional): a KCC-20 on the same reference program with its own KAS book and USD reference, quoted by the
   * market maker, traded by the traders and paired with `token` by pair orders (token2 / token: KobPair, KobCondPair, KobIfdPair)
   */
  token2?: Token2Config;
  /** a third soak token (optional), configured and handled exactly like `token2` (2026-10-03: TBTC, Binance BTCUSDT / KASUSDT) */
  token3?: Token2Config;
  price: {
    /** Binance USD-M futures KASUSDT (Binance has no KAS spot market) */
    url: string;
    pollMs: number;
    /** fall back to the last good price for at most this long before the bots pause */
    staleMs: number;
  };
  bank: {
    key: string;
    /** a wallet is topped up when its spendable KAS falls below `min`, to `target` (KAS), in chunks of `chunk` KAS */
    wallets: Record<string, { min: number; target: number }>;
    chunkKas: number;
    /** consolidate coinbase dust when more than this many UTXOs are spendable */
    consolidateAbove: number;
    intervalSec: number;
  };
  mm: {
    key: string;
    levels: number;
    /** first level distance from the reference, basis points */
    innerBps: number;
    /** step between levels, basis points */
    stepBps: number;
    /** size of one ladder level, whole tokens of the book (decimal strings): drawn in token base units between the two */
    minTokens: string;
    maxTokens: string;
    /** re-quote a level when its price is this far from where it should be, basis points (with `requoteMinBps`: the most for any level) */
    requoteBps: number;
    /**
     * tight ladders: re-quote a level once it drifts by its own distance from the mid, at least this many bps (mm.ts `requoteThreshold`);
     * absent = `requoteBps` for every level
     */
    requoteMinBps?: number;
    /** the quotes skew by this many bps per whole token of inventory gained (or lost) since the start */
    skewBpsPerToken: number;
    /** the most the inventory skew moves the ladder's mid from the reference, bps (default 150; a tight ladder that tracks the reference: a few bps) */
    maxSkewBps?: number;
    intervalSec: number;
  };
  /**
   * token-UTXO consolidation of the bots' own wallets (market maker first, traders too): per key and token, when more than
   * (fan-out target + slack) plain UTXOs are held, the smallest excess ones are merged (up to the program's token inputs per
   * transaction), never below the fan-out target; at most maxTxPerInterval merge transactions per intervalSec per key and token.
   * Every field is optional (defaults: enabled, slack 8, 3 per 60 s, traders on).
   */
  consolidate?: Partial<ConsolidateConfig>;
  traders: TraderConfig[];
  /**
   * Lag tolerance in seconds of the "wait until the indexer follows" gates (default 30; 0: strict). The bots' gate (indexer-gate.ts)
   * trades while the indexer lags by at most this much, and the supervisor writes it as `max_lag_secs` into every executor.toml (the
   * executors' matcher, keepers and maintenance plan within it; per executor: `executors[].maxLagSecs`).
   */
  maxLagSecs?: number;
  x402: {
    enabled: boolean;
    payerKey: string;
    merchantKey: string;
    /** paywall HTTP server */
    listen: string;
    facilitatorUrl: string;
    meanIntervalSec: number;
    /**
     * Intent payments of invoices (facilitator `intents` + `invoices`, docs/ops/executor.md A.7): every fourth payer turn the merchant
     * registers an invoice of `amountKas` KAS whose only entry is an intent swap offer paid with TUSD (KCC20Ref), and the payer pays
     * it with `payInvoiceWithIntent` (one signed creation; the facilitator executes the router intent against the TUSD bids).
     * The supervisor switches the facilitator's `intents` / `invoices` on when this is enabled (keeper = the executor's own key).
     */
    invoiceIntent?: { enabled: boolean; amountKas?: number; maxSellTusd?: number };
  };
  checker: {
    intervalSec: number;
  };
  /** read-only web UI server (scripts/serve-ui.mjs), supervised when set */
  ui?: { listen: string; indexerUrl?: string };
  /**
   * Dynamic priority fee of every bot transaction and the bank (web `kob/fee-policy.ts`): the rate comes from the node's `getFeeEstimate` bucket of the
   * action's urgency (IOC / FOK / market / marketable limits and x402 payments: priority; resting quotes, amends, cancels: normal; consolidation,
   * fan-out, bank payouts: low), clamped to [100, maxRate] sompi per gram, with a once-only rebuild at a lower rate when one transaction would pay more
   * than maxFeeKas. An absent `fees` key means the defaults below (dynamic ON); `"dynamic": false` is the old behaviour (every transaction at the 100
   * sompi per gram relay floor). The checker accepts any recorded rate in [100, maxRate] whose fee is exactly rate x mass.
   */
  fees?: { dynamic?: boolean; maxRate?: number; maxFeeKas?: number };
}

/** The config / state slots of the soak tokens beside TUSD, in order (each: config `tokenN`, state `tokenN` + `genesisOutputsN`). */
export const ASSET_SLOTS = ['token2', 'token3'] as const;
export type AssetSlot = (typeof ASSET_SLOTS)[number];
export type TokenSlot = 'token' | AssetSlot;
export const TOKEN_SLOTS: readonly TokenSlot[] = ['token', ...ASSET_SLOTS];
export const isTokenSlot = (s: string): s is TokenSlot => (TOKEN_SLOTS as readonly string[]).includes(s);

/** The Binance symbol of a reference URL (`...?symbol=BTCUSDT`), or null. */
export function symbolOfUrl(url: string): string | null {
  try {
    return new URL(url).searchParams.get('symbol');
  } catch {
    return null;
  }
}

export interface LoadedConfig extends SoakConfig {
  /** the fee policy settings with the defaults filled in (dynamic ON, maxRate 1000 sompi per gram, maxFeeKas 1) */
  feeSettings: FeeSettings;
  /** absolute run directory */
  runPath: string;
  configPath: string;
}

/**
 * The fee settings of a config: an absent `fees` (or absent keys) means the defaults (dynamic ON). A present but invalid value throws: the operator
 * must not find out from a transaction that a typo silently changed what the soak pays.
 */
export function feeSettingsOf(fees: SoakConfig['fees']): FeeSettings {
  const out: FeeSettings = { ...DEFAULT_FEES };
  if (fees === undefined || fees === null) return out;
  if (typeof fees !== 'object' || Array.isArray(fees)) throw new Error('config.fees must be an object { dynamic, maxRate, maxFeeKas }');
  if (fees.dynamic !== undefined) {
    if (typeof fees.dynamic !== 'boolean') throw new Error('config.fees.dynamic must be true or false');
    out.dynamic = fees.dynamic;
  }
  if (fees.maxRate !== undefined) {
    const r = parseMaxRate(fees.maxRate);
    if (r === null) throw new Error('config.fees.maxRate must be a whole number of sompi per gram from 100 to 1000000');
    out.maxRate = r;
  }
  if (fees.maxFeeKas !== undefined) {
    const k = parseMaxFeeKas(fees.maxFeeKas);
    if (k === null) throw new Error('config.fees.maxFeeKas must be 0 (no total cap) to 1000 KAS with at most 8 decimals');
    out.maxFeeKas = k;
  }
  return out;
}

export function loadConfig(path: string): LoadedConfig {
  const configPath = resolve(path);
  const raw = JSON.parse(readFileSync(configPath, 'utf8')) as SoakConfig;
  const base = dirname(configPath);
  const abs = (p: string) => (isAbsolute(p) ? p : resolve(base, p));
  return {
    ...raw,
    feeSettings: feeSettingsOf(raw.fees),
    runPath: abs(raw.runDir),
    kobWasmDir: abs(raw.kobWasmDir),
    kaspaSdkDir: abs(raw.kaspaSdkDir),
    executorBin: abs(raw.executorBin),
    minerBin: abs(raw.minerBin),
    configPath,
  };
}
