// Market maker: a ladder of GTC limit bids and asks around the Binance-derived reference, skewed by the inventory it accumulated since
// start. Levels drifting more than `requoteBps` from where they belong are amended (cancel-replace, one transaction); surplus orders are
// cancelled; a large reference move (or every ~30 minutes) cancels the whole ladder at once (cancel-all) and rebuilds it.
import type { OrderView } from '@/data/indexer-types';
import type { TokenMarket } from '@/kob/plan-types';
import type { SoakConfig } from '../config';
import type { Env } from '../env';
import { errText, type Logger, type Stats } from '../log';
import { myOrders, readBook, tokenRef } from '../market';
import type { RefSource } from '../price';
import { bps, onTick, randUnits, sleep, unitsOf } from '../util';
import { walletFor, type BotWallet } from '../wallet';
import { amend, cancel, cancelAll, priceOf, place } from './common';
import { consolidateTokens } from './consolidate';
import { consolidateConfig } from './consolidate-math';
import { fanoutTarget } from './fanout';
import { requoteThreshold } from './mm-math';

interface Level {
  side: 'buy' | 'sell';
  rank: number;
  /** sompi per whole token */
  price: bigint;
  /** base units */
  amount: bigint;
}

export class MarketMaker {
  readonly w: BotWallet;
  private baseTokens: bigint | null = null;
  private lastFullRebuild = Date.now();
  private lastRef: bigint | null = null;
  /** the reference of the previous tick: which side the ladder moves away from first */
  private prevTickRef: bigint | null = null;
  private stopped = false;

  private readonly c: SoakConfig['mm'];
  /** '' for the primary book, '@<ticker>' for another (stat and action tags) */
  private readonly sfx: string;

  constructor(
    private readonly env: Env,
    private readonly feed: RefSource,
    private readonly m: TokenMarket,
    private readonly log: Logger,
    readonly stats: Stats,
    overrides: Partial<SoakConfig['mm']> = {},
    primary = true,
  ) {
    this.c = { ...env.cfg.mm, ...overrides };
    this.sfx = primary ? '' : `@${m.ticker}`;
    this.w = walletFor(env, this.c.key, stats, log);
  }

  stop(): void {
    this.stopped = true;
  }

  async run(): Promise<void> {
    while (!this.stopped) {
      // no ladder work against a book that lags the chain: wait here instead of planning and being refused at submit
      if (!(await this.env.gate?.waitOpen(() => this.stopped) ?? true)) break;
      try {
        await this.tick();
      } catch (e) {
        this.stats.inc(`mm_errors${this.sfx}`);
        this.log.warn('mm tick failed', { error: errText(e) });
      }
      // after the ladder, in the same loop: this book's placements and the merge never plan over the same token UTXOs
      await consolidateTokens(this.env, this.w, this.m, fanoutTarget(this.env, 'mm', this.m), consolidateConfig(this.env.cfg.consolidate));
      await sleep(this.c.intervalSec * 1000);
    }
  }

  /** tokens held by the MM: P2PK balance plus the custody of its open asks, in base units */
  private async inventory(mine: OrderView[]): Promise<bigint> {
    const m = this.m;
    const free = await this.w.tokenBalance(tokenRef(this.env, m));
    const inAsks = mine.filter((o) => o.side === 1).reduce((s, o) => s + BigInt(o.amount_left ?? 0), 0n); // amount_left: base units still in the ask's custody
    return free + inAsks;
  }

  private ladder(ref: bigint, skewBps: number): Level[] {
    const c = this.c;
    const tick = this.m.tick;
    const mid = ref + bps(ref, skewBps);
    // a level's size: any amount of base units between minTokens and maxTokens whole tokens (no step)
    const lo = unitsOf(c.minTokens, this.m.decimals);
    const hi = unitsOf(c.maxTokens, this.m.decimals);
    const out: Level[] = [];
    for (let i = 0; i < c.levels; i++) {
      const d = c.innerBps + i * c.stepBps;
      const amount = randUnits(lo, hi);
      out.push({ side: 'buy', rank: i, price: onTick(mid - bps(mid, d), tick), amount });
      out.push({ side: 'sell', rank: i, price: onTick(mid + bps(mid, d), tick), amount });
    }
    return out;
  }

  private async tick(): Promise<void> {
    const c = this.c;
    const x = this.sfx;
    const ref = this.feed.get();
    if (!ref) {
      this.stats.inc(`mm_no_price${x}`);
      return;
    }
    const mine = (await myOrders(this.env, this.w.pk, this.m)).filter((o) => (o.contract === 'KobAsk' || o.contract === 'KobBid') && o.tif === 0 && o.token === this.m.covenantId);
    const inv = await this.inventory(mine);
    if (this.baseTokens === null) this.baseTokens = inv;
    // whole tokens gained (or lost) since the start
    const excess = Number(inv - this.baseTokens) / 10 ** this.m.decimals;
    // long inventory -> quote lower (sell more, buy less); short -> higher
    const cap = c.maxSkewBps ?? 150;
    const skew = Math.max(-cap, Math.min(cap, -excess * c.skewBpsPerToken));
    this.stats.set(`mm_inventory_delta_tokens${x}`, excess);
    this.stats.set(`mm_skew_bps${x}`, skew);
    this.stats.set(`mm_open_orders${x}`, mine.length);

    const moved = this.lastRef ? Math.abs(Number(ref.perToken - this.lastRef)) / Number(this.lastRef) : 0;
    if (mine.length && (moved > 0.02 || Date.now() - this.lastFullRebuild > 30 * 60_000)) {
      const n = await cancelAll(this.env, this.w, mine, `mm_cancelAll${x}`);
      this.log.info('mm ladder reset (cancel-all)', { book: this.m.ticker, moved: moved.toFixed(4), cancelled: n });
      this.lastFullRebuild = Date.now();
      this.lastRef = ref.perToken;
      return;
    }
    this.lastRef ??= ref.perToken;

    const want = this.ladder(ref.perToken, skew);
    const book = await readBook(this.env, this.m);
    let txs = 0;
    const maxTx = 4;
    // a tight ladder moves the side the reference moves AWAY from first (up: asks first), so a re-quoted level never meets its own stale
    // opposite quote (a self-trade the planner refuses)
    const up = this.prevTickRef !== null && ref.perToken > this.prevTickRef;
    this.prevTickRef = ref.perToken;
    for (const side of up ? (['sell', 'buy'] as const) : (['buy', 'sell'] as const)) {
      const have = mine
        .filter((o) => (side === 'buy' ? o.side === 2 : o.side === 1))
        .map((o) => ({ o, p: priceOf(o) ?? 0n }))
        .sort((a, b) => (side === 'buy' ? Number(b.p - a.p) : Number(a.p - b.p)));
      const targets = want.filter((l) => l.side === side);
      for (let i = 0; i < targets.length && txs < maxTx; i++) {
        const t = targets[i];
        const cur = have[i];
        if (!cur) {
          const r = await place(this.env, this.w, { type: 'limit', side, amount: t.amount, price: t.price, crossing: 'limit', ...(side === 'buy' ? { maxFills: 3n } : {}) }, `mm_${side}${x}`, { book, mine }, this.m);
          if (r.ok || r.error) txs++;
          continue;
        }
        const off = Math.abs(Number(cur.p - t.price)) / Number(t.price);
        if (off * 10_000 > requoteThreshold(c, t.rank)) {
          const r = await amend(this.env, this.w, cur.o, t.price);
          this.stats.inc(r.ok ? `mm_amend_ok${x}` : `mm_amend_failed${x}`);
          // an amended bid keeps its KAS budget: a partly filled bid moved up may no longer fund one minimum fill -> cancel, re-place next tick
          if (!r.ok && !r.conflict && !r.paused && r.error !== 'order is gone') await cancel(this.env, this.w, cur.o, `mm_cancel${x}`);
          txs++;
        }
      }
      // surplus orders beyond the ladder
      for (const extra of have.slice(targets.length)) {
        if (txs >= maxTx) break;
        await cancel(this.env, this.w, extra.o, `mm_cancel${x}`);
        txs++;
      }
    }
    this.stats.set(`mm_ref_per_token${x}`, ref.perToken.toString());
  }
}

