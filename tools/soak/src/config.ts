// Soak configuration (JSON).
// `config.example.json` is the committed template. The live copy is run/config.json.
import { readFileSync } from 'node:fs';
import { dirname, isAbsolute, resolve } from 'node:path';
// relative (not `@/`): the fee settings are unit-tested by node --test, which has no path alias
import { DEFAULT_FEES, parseMaxFeeKas, parseMaxRate, type FeeSettings } from '../../../web/src/kob/fee-policy.ts';
import type { Bands } from './bots/bands';
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
  /**
   * Extra start flags of `kobd --executor`. `--lean` keeps it the Executor alone. Without it, it is
   * also the Indexer.
   */
  args?: string[];
}

export interface TraderConfig {
  /** key name in run/keys.json */
  key: string;
  /** mean seconds between actions (exponential inter-arrival) */
  meanIntervalSec: number;
  /**
   * Relative weights per action (`DEFAULT_WEIGHTS` of bots/weights.ts). A missing action keeps its
   * default weight and 0 switches it off.
   */
  weights?: Record<string, number>;
  /**
   * The largest order of this trader, in USD (= TUSD; on an asset book, its value at the Binance
   * reference).
   * Sizes are drawn continuously in token base units, never rounded to a step, mostly between half
   * a dollar and this value.
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

/**
 * A soak token beside TUSD (config `token2`, `token3`).
 * It has its own KAS book, a USD reference from Binance and pair orders with TUSD.
 */
export interface Token2Config extends TokenConfig {
  /**
   * USD price of one whole token (Binance public REST returning `{price}`).
   * The KAS reference of one whole token is usd / KASUSDT. Only the bots read it.
   */
  priceUrl: string;
  /**
   * Share of the trader actions placed on this token's KAS book. The other non-pair actions trade
   * `token`.
   */
  bookShare: number;
  /**
   * Share of the trader actions that are pair orders (every pair kind, `pairWeights`) of this token
   * (A) against `token` (B).
   */
  pairShare?: number;
  /** older spelling of `pairShare` (the retired cross limits) */
  crossShare?: number;
  /**
   * Weights of the pair actions (default `DEFAULT_PAIR_WEIGHTS` of bots/weights.ts). 0 switches one
   * off.
   */
  pairWeights?: Record<string, number>;
  /** market-maker overrides for this book (default: the `mm` section) */
  mm?: Partial<SoakConfig['mm']>;
  /** size of one pair order: its USD value, drawn in this range (default [3, 20]) */
  pairUsd?: [number, number];
  /** older spelling of `pairUsd` */
  crossUsd?: [number, number];
  /**
   * Token fan-out per holder, for the market maker and the traders: the number of outputs and the
   * whole tokens per output (a decimal string).
   */
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
   * The miner child. scripts/miner-config.mjs validates it and writes it to run/miner.json, which
   * the miner re-reads on change.
   * It sets the backend, intermittent mining between `lowKas` and `highKas` of the bank's spendable
   * KAS, and the duty cycle.
   * Absent means continuous mining.
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
   * The second soak token (optional). It is a KCC-20 on the same reference program with its own KAS
   * book and USD reference.
   * The market maker quotes it, the traders trade it, and pair orders pair it with `token` (token2
   * / token: KobPair, KobCondPair, KobIfdPair).
   */
  token2?: Token2Config;
  /**
   * A third soak token (optional), configured and handled exactly like `token2`, for example TBTC
   * with Binance BTCUSDT / KASUSDT.
   */
  token3?: Token2Config;
  /**
   * Tokens issued elsewhere that the soak lists beside its own, for example a Kasseal launchpad
   * token of program KCC20PublicMint.
   * They are written to the soak registry (the executors' allowlist and the UI's token list), so
   * they can be traded by hand.
   * The traders also trade the KAS book of those with a `trade` block, such as a KRON token minted
   * on TN10 by KRON's own tooling. The soak cannot issue KRON.
   * The market maker does not quote them.
   */
  externalTokens?: ExternalTokenConfig[];
  /**
   * Launchpad token programs whose templates the soak registry lists, so the executors list KOB
   * orders of any token of them
   * (src/registry-templates.ts `TEST_PROGRAM_TEMPLATES`).
   * KForgeToken1785 and KaspaRocketToken11458 are embedded in KOB as pending review (testnet-10
   * only). They are listed here as reviewed for a TN10 test.
   * KForgeToken7214 is reviewed in the shipped registry too.
   */
  testPrograms?: ('KForgeToken1785' | 'KaspaRocketToken11458' | 'KForgeToken7214')[];
  price: {
    /** Binance USD-M futures KASUSDT (Binance has no KAS spot market) */
    url: string;
    pollMs: number;
    /** fall back to the last good price for at most this long before the bots pause */
    staleMs: number;
  };
  bank: {
    key: string;
    /**
     * A wallet is topped up when its spendable KAS falls below `min`. It is topped up to `target`
     * (KAS) in chunks of `chunk` KAS.
     */
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
    /**
     * Size of one ladder level, in whole tokens of the book (decimal strings). It is drawn in token
     * base units between the two.
     */
    minTokens: string;
    maxTokens: string;
    /**
     * Re-quote a level when its price is this far from where it should be, in basis points. With
     * `requoteMinBps` this is the most for any level.
     */
    requoteBps: number;
    /**
     * For tight ladders: re-quote a level once it drifts by its own distance from the mid, but by
     * at least this many bps (mm.ts `requoteThreshold`).
     * Absent means `requoteBps` for every level.
     */
    requoteMinBps?: number;
    /**
     * The quotes skew by this many bps per whole token of inventory gained (or lost) since the
     * start.
     */
    skewBpsPerToken: number;
    /**
     * The most the inventory skew moves the ladder's mid from the reference, in bps (default 150).
     * A tight ladder that tracks the reference uses a few bps.
     */
    maxSkewBps?: number;
    intervalSec: number;
  };
  /**
   * Token-UTXO consolidation of the bots' own wallets (market maker first, traders too).
   * Per key and token, when more than (fan-out target + slack) plain UTXOs are held, the smallest
   * excess ones are merged.
   * A merge takes up to the program's token inputs per transaction and never goes below the fan-out
   * target.
   * At most maxTxPerInterval merge transactions run per intervalSec per key and token.
   * Every field is optional (defaults: enabled, slack 8, 3 per 60 s, traders on).
   */
  consolidate?: Partial<ConsolidateConfig>;
  traders: TraderConfig[];
  /**
   * How far from the reference the traders let a taker or an auction go, in bps (bots/bands.ts
   * `DEFAULT_BANDS`, every field optional).
   * Market and stop auctions fill at their far bound, and the executors follow the chain 1 to 2
   * minutes late.
   * So the bound is what sets the chart's wicks.
   */
  bands?: Partial<Bands>;
  /**
   * Lag tolerance in seconds of the "wait until the indexer follows" gates (default 30, 0 is
   * strict).
   * The bots' gate (indexer-gate.ts) trades while the indexer lags by at most this much.
   * The supervisor writes it as `max_lag_secs` into every executor.toml, so the executors' matcher,
   * keepers and maintenance plan within it.
   * It can be set per executor with `executors[].maxLagSecs`.
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
     * Intent payments of invoices (facilitator `intents` + `invoices`, docs/ops/executor.md A.7).
     * Every fourth payer turn the merchant registers an invoice of `amountKas` KAS.
     * Its only entry is an intent swap offer paid with TUSD (KCC20Ref).
     * The payer pays it with `payInvoiceWithIntent` (one signed creation), and the facilitator
     * executes the router intent against the TUSD bids.
     * The supervisor switches the facilitator's `intents` / `invoices` on when this is enabled
     * (keeper = the executor's own key).
     */
    invoiceIntent?: { enabled: boolean; amountKas?: number; maxSellTusd?: number };
    /**
     * Weights of the payer's turns (bots/x402-retry.ts `DEFAULT_X402_WEIGHTS`, missing means
     * default).
     * The kinds are:
     * - `/native`, `/token`, `/swap` and `invoice`.
     * - `/retry/drop`: the merchant drops the first paid request.
     * - `/retry/lost`: the merchant loses the settled answer.
     * - `/retry/conflict`: the market maker amends the quoted bid away (order_conflict, rebuild).
     * 0 switches a kind off.
     */
    weights?: Record<string, number>;
  };
  checker: {
    intervalSec: number;
  };
  /** read-only web UI server (scripts/serve-ui.mjs), supervised when set */
  ui?: { listen: string; indexerUrl?: string };
  /**
   * Dynamic priority fee of every bot transaction and the bank (web `kob/fee-policy.ts`).
   * The rate comes from the node's `getFeeEstimate` bucket of the action's urgency:
   * - priority: IOC / FOK / market / marketable limits and x402 payments.
   * - normal: resting quotes, amends, cancels.
   * - low: consolidation, fan-out, bank payouts.
   * The rate is clamped to [100, maxRate] sompi per gram.
   * A once-only rebuild at a lower rate happens when one transaction would pay more than maxFeeKas.
   * An absent `fees` key means the defaults below (dynamic ON).
   * `"dynamic": false` pays the 100 sompi per gram relay floor on every transaction.
   * The checker accepts any recorded rate in [100, maxRate] whose fee is exactly rate x mass.
   */
  fees?: { dynamic?: boolean; maxRate?: number; maxFeeKas?: number };
}

/**
 * The config / state slots of the soak tokens beside TUSD, in order (each: config `tokenN`, state
 * `tokenN` + `genesisOutputsN`).
 */
/** A token the soak lists but did not issue (`externalTokens`). */
export interface ExternalTokenConfig {
  ticker: string;
  name: string;
  covenantId: string;
  /**
   * Its token program: the reference KCC-20 3 / 3 (`KCC20Ref`), the reference's published
   * public-mint build (`KCC20PublicMint`),
   * or one of the two reviewed KRON programs (`KronToken2433`, `KronToken2732`: 46-byte state, no
   * extension commitment, 4 / 5 token slots).
   */
  program: 'KCC20Ref' | 'KCC20PublicMint' | 'KronToken2433' | 'KronToken2732' | 'KForgeToken1785' | 'KaspaRocketToken11458' | 'KForgeToken7214';
  decimals: number;
  /** 64-hex, default all zero (KCC-20 only; a KRON token has none) */
  extensionCommitment?: string;
  description?: string;
  /**
   * The traders trade this token's KAS book (bots/traders.ts: the KAS-book actions of their mix, no
   * pair orders).
   */
  trade?: ExternalTradeConfig;
}

/** How the traders trade an external token (`externalTokens[].trade`). */
export interface ExternalTradeConfig {
  /**
   * Share of the trader actions on this token's KAS book (drawn before the asset books'
   * `bookShare`).
   */
  bookShare: number;
  /**
   * The KAS reference of one whole token (decimal KAS, e.g. "0.05").
   * There is no Binance price for it. The traders price around the book's touch midpoint when it
   * lies within 20 % of this value, else around this value.
   */
  refKas: string;
}

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
  /**
   * The fee policy settings with the defaults filled in (dynamic ON, maxRate 1000 sompi per gram,
   * maxFeeKas 1).
   */
  feeSettings: FeeSettings;
  /** absolute run directory */
  runPath: string;
  configPath: string;
}

/**
 * The fee settings of a config. An absent `fees` (or absent keys) means the defaults (dynamic ON).
 * A present but invalid value throws, so a typo cannot silently change what the soak pays.
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
