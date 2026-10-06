// Reference price: Binance USD-M futures KASUSDT (public REST, no key; Binance has no KAS spot market). 1 TUSD = 1 USD worth of KAS,
// so the reference price of one TUSD in KAS is 1 / KASUSDT. Only the bots read it; nothing on chain depends on it.
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import type { LoadedConfig } from './config';
import { errText, type Logger, type Stats } from './log';

export interface RefPrice {
  kasUsd: number;
  /** sompi per whole token (the order price unit: sompi per `scale` = 10^decimals base units) */
  perToken: bigint;
  /** USD per whole token (TUSD: 1) */
  usd: number;
  ts: number;
}

/** Called with every good poll: the Binance symbol, its USD price and the exchange's time (else the local time), unix ms. */
export type OnPrice = (symbol: string, usd: number, ts: number) => void;

/** Binance's `time` of a ticker answer when it is a plausible unix ms, else now. */
const exchangeTime = (j: { time?: unknown }): number => {
  const t = Number(j.time);
  return Number.isSafeInteger(t) && Math.abs(t - Date.now()) < 10 * 60_000 ? t : Date.now();
};

export class PriceFeed implements RefSource {
  private last: RefPrice | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  constructor(
    private readonly cfg: LoadedConfig,
    private readonly log: Logger,
    private readonly stats: Stats,
    private readonly onPrice: OnPrice | null = null,
  ) {}

  async start(): Promise<void> {
    await this.poll();
    this.timer = setInterval(() => void this.poll(), this.cfg.price.pollMs);
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
  }

  private async poll(): Promise<void> {
    try {
      const r = await fetch(this.cfg.price.url, { signal: AbortSignal.timeout(8000) });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const j = (await r.json()) as { price?: string; symbol?: string; time?: number };
      const kasUsd = Number(j.price);
      if (!Number.isFinite(kasUsd) || kasUsd <= 0) throw new Error(`bad price ${j.price}`);
      // one TUSD = one USD worth of KAS
      const perToken = BigInt(Math.round((1 / kasUsd) * 1e8));
      this.last = { kasUsd, perToken, usd: 1, ts: Date.now() };
      this.onPrice?.(j.symbol ?? 'KASUSDT', kasUsd, exchangeTime(j));
      this.stats.set('ref_kas_usd', kasUsd);
      this.stats.set('ref_per_token_sompi', perToken.toString());
    } catch (e) {
      this.stats.inc('price_errors');
      this.log.warn('price poll failed', { error: errText(e) });
    }
  }

  /** the latest reference, or null when it is older than `staleMs` (bots pause) */
  get(): RefPrice | null {
    if (!this.last || Date.now() - this.last.ts > this.cfg.price.staleMs) return null;
    return this.last;
  }
}

/** Anything the bots read a per-token KAS reference from. */
export interface RefSource {
  get(): RefPrice | null;
}

/** USD price of one whole asset token from a Binance ticker (`{price}`), polled like the KAS feed. */
export class UsdFeed {
  private last: { usd: number; ts: number } | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  constructor(
    private readonly url: string,
    private readonly pollMs: number,
    private readonly staleMs: number,
    private readonly name: string,
    private readonly log: Logger,
    private readonly stats: Stats,
    private readonly onPrice: OnPrice | null = null,
  ) {}

  async start(): Promise<void> {
    await this.poll();
    this.timer = setInterval(() => void this.poll(), this.pollMs);
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
  }

  private async poll(): Promise<void> {
    try {
      const r = await fetch(this.url, { signal: AbortSignal.timeout(8000) });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const j = (await r.json()) as { price?: string; symbol?: string; time?: number };
      const usd = Number(j.price);
      if (!Number.isFinite(usd) || usd <= 0) throw new Error(`bad price ${j.price}`);
      this.last = { usd, ts: Date.now() };
      if (j.symbol) this.onPrice?.(j.symbol, usd, exchangeTime(j));
      this.stats.set(`ref_usd:${this.name}`, usd);
    } catch (e) {
      this.stats.inc(`price_errors:${this.name}`);
      this.log.warn('price poll failed', { feed: this.name, error: errText(e) });
    }
  }

  get(): { usd: number; ts: number } | null {
    if (!this.last || Date.now() - this.last.ts > this.staleMs) return null;
    return this.last;
  }
}

/**
 * The KAS reference of one whole USD-priced token: assetUsd / KASUSDT (e.g. 1 TETH at ETHUSDT 2690 and KASUSDT 0.0435 = 61,839 KAS).
 * Null (bots pause on this book) when either feed is stale.
 */
export class DerivedFeed implements RefSource {
  constructor(
    private readonly kas: PriceFeed,
    private readonly asset: UsdFeed,
  ) {}

  get(): RefPrice | null {
    const k = this.kas.get();
    const a = this.asset.get();
    if (!k || !a) return null;
    return { kasUsd: k.kasUsd, perToken: BigInt(Math.round((a.usd / k.kasUsd) * 1e8)), usd: a.usd, ts: Math.min(k.ts, a.ts) };
  }
}

/**
 * The reference record the soak UI server serves (scripts/serve-ui.mjs `/soak/ref/v1/*`, read-only): every good Binance poll of the bots
 * appends `{"t":<unix ms>,"s":"<symbol>","p":<USD>}` to run/state/ref-prices.jsonl, and run/state/ref-symbols.json says which soak market
 * each symbol is the reference of (KASUSDT: KAS itself, quoted by the USD token TUSD; ETHUSDT: TETH; BTCUSDT: TBTC). The web app overlays
 * these lines on its USD charts (config `referenceFeedUrl`), so how well the soak tracks the real markets is visible.
 */
export class RefRecorder {
  private readonly file: string;
  constructor(runPath: string) {
    const dir = join(runPath, 'state');
    mkdirSync(dir, { recursive: true });
    this.file = join(dir, 'ref-prices.jsonl');
  }

  static writeSymbols(runPath: string, symbols: { symbol: string; ticker: string; covenantId: string | null; role: 'kas' | 'asset' }[], usdToken: string): void {
    const dir = join(runPath, 'state');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'ref-symbols.json'), JSON.stringify({ usdToken, symbols }, null, 1) + '\n');
  }

  readonly onPrice: OnPrice = (symbol, usd, ts) => {
    try {
      appendFileSync(this.file, JSON.stringify({ t: ts, s: symbol, p: usd }) + '\n');
    } catch {
      /* the record is best effort: never stops the bots */
    }
  };
}
