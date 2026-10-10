// Market maker: a ladder of GTC limit bids and asks around the Binance-derived reference, skewed by the inventory it accumulated since
// start. Levels drifting more than `requoteBps` from where they belong are amended (cancel-replace, one transaction); surplus orders are
// cancelled; a large reference move (or every ~30 minutes) cancels the whole ladder at once (cancel-all) and rebuilds it. The indexer lags the
// chain by minutes under load: an order the maker just placed (not listed yet) or amended (listed with its spent UTXO) holds its level until
// the listing catches up, so a level is never placed twice and an amended order is never amended again from its stale view.
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
import { ladderSteps, requoteThreshold, type LadderSlot } from './mm-math';

/** how long an order this maker placed or cancelled is remembered while the indexer does not reflect it (the wallet's reservation time) */
const PENDING_MS = 600_000;
/** a placement younger than this is not looked up yet (the indexer cannot list it so soon) */
const LOOKUP_AFTER_MS = 15_000;
/** transactions per tick and side of the ladder (one side never starves the other) */
const TX_PER_SIDE = 2;

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
  /** orders this ladder placed that the indexer does not list yet: covenant id -> side, price, time */
  private readonly placed = new Map<string, { side: 'buy' | 'sell'; price: bigint; at: number }>();
  /** orders this ladder cancelled (cancel, cancel-all): while their cancel is pending they hold no level */
  private readonly dropping = new Map<string, number>();

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
      const cc = consolidateConfig(this.env.cfg.consolidate);
      await consolidateTokens(this.env, this.w, this.m, fanoutTarget(this.env, 'mm', this.m), cc);
      // the KAS the fills pay back in small coins: merged, so a bid's funding stays within one plan's inputs
      if (cc.enabled) await this.w.mergeKas({ above: cc.kasAbove, maxInputs: cc.kasInputs });
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
    // the listing after this maker's own pending transactions: `live` may be acted on; an amended order (or one a matcher's fill in the
    // mempool spends) holds its level; a cancelled one is gone
    const now = Date.now();
    for (const [id, at] of this.dropping) if (now - at > PENDING_MS) this.dropping.delete(id);
    const { live, pending } = this.w.pendingSplit(mine);
    const held = pending.filter((o) => !this.dropping.has(o.covenant_id));
    await this.prunePlaced(mine, now);
    if (this.baseTokens === null) this.baseTokens = inv;
    // whole tokens gained (or lost) since the start
    const excess = Number(inv - this.baseTokens) / 10 ** this.m.decimals;
    // long inventory -> quote lower (sell more, buy less); short -> higher
    const cap = c.maxSkewBps ?? 150;
    const skew = Math.max(-cap, Math.min(cap, -excess * c.skewBpsPerToken));
    this.stats.set(`mm_inventory_delta_tokens${x}`, excess);
    this.stats.set(`mm_skew_bps${x}`, skew);
    this.stats.set(`mm_open_orders${x}`, mine.length);
    this.stats.set(`mm_pending_orders${x}`, held.length + this.placed.size);

    const moved = this.lastRef ? Math.abs(Number(ref.perToken - this.lastRef)) / Number(this.lastRef) : 0;
    if (live.length && (moved > 0.02 || Date.now() - this.lastFullRebuild > 30 * 60_000)) {
      for (const o of live) this.dropping.set(o.covenant_id, now);
      const n = await cancelAll(this.env, this.w, live, `mm_cancelAll${x}`);
      this.log.info('mm ladder reset (cancel-all)', { book: this.m.ticker, moved: moved.toFixed(4), cancelled: n });
      this.lastFullRebuild = Date.now();
      this.lastRef = ref.perToken;
      return;
    }
    this.lastRef ??= ref.perToken;

    const want = this.ladder(ref.perToken, skew);
    const book = await readBook(this.env, this.m);
    // a tight ladder moves the side the reference moves AWAY from first (up: asks first), so a re-quoted level never meets its own stale
    // opposite quote (a self-trade the planner refuses)
    const up = this.prevTickRef !== null && ref.perToken > this.prevTickRef;
    this.prevTickRef = ref.perToken;
    for (const side of up ? (['sell', 'buy'] as const) : (['buy', 'sell'] as const)) {
      const own = (o: OrderView) => (side === 'buy' ? o.side === 2 : o.side === 1);
      const slots: LadderSlot<OrderView>[] = [
        ...live.filter(own).map((o) => ({ price: priceOf(o) ?? 0n, order: o })),
        ...held.filter(own).map((o) => ({ price: priceOf(o) ?? 0n, order: null })),
        ...[...this.placed.values()].filter((p) => p.side === side).map((p) => ({ price: p.price, order: null })),
      ];
      const targets = want.filter((l) => l.side === side);
      for (const step of ladderSteps(side, targets, slots, (rank) => requoteThreshold(c, rank), TX_PER_SIDE)) {
        if (step.kind === 'place') {
          const t = step.target;
          const r = await place(this.env, this.w, { type: 'limit', side, amount: t.amount, price: t.price, crossing: 'limit', ...(side === 'buy' ? { maxFills: 3n } : {}) }, `mm_${side}${x}`, { book, mine: live }, this.m);
          if (r.ok) for (const id of r.created ?? []) this.placed.set(id, { side, price: t.price, at: Date.now() });
        } else if (step.kind === 'amend') {
          const r = await amend(this.env, this.w, step.order, step.target.price);
          this.stats.inc(r.ok ? `mm_amend_ok${x}` : `mm_amend_failed${x}`);
          // an amended bid keeps its KAS budget: a partly filled bid moved up may no longer fund one minimum fill -> cancel, re-place next tick
          if (!r.ok && !r.conflict && !r.paused && r.error !== 'order is gone') await this.drop(step.order);
        } else {
          // surplus orders beyond the ladder
          await this.drop(step.order);
        }
      }
    }
    this.stats.set(`mm_ref_per_token${x}`, ref.perToken.toString());
  }

  /** cancels one order of the ladder; while the cancel is pending the order holds no level */
  private async drop(o: OrderView): Promise<void> {
    const r = await cancel(this.env, this.w, o, `mm_cancel${this.sfx}`);
    if (r.ok) this.dropping.set(o.covenant_id, Date.now());
  }

  /**
   * Forgets the placements the indexer reflects: listed as active (then `mine` has them), or known in any other status (filled or killed before
   * the listing caught up); a placement never seen is dropped after PENDING_MS (its transaction did not make it).
   */
  private async prunePlaced(mine: OrderView[], now: number): Promise<void> {
    const listed = new Set(mine.map((o) => o.covenant_id));
    let lookups = 0;
    for (const [id, p] of this.placed) {
      if (listed.has(id) || now - p.at > PENDING_MS) {
        this.placed.delete(id);
        continue;
      }
      if (now - p.at < LOOKUP_AFTER_MS || lookups >= 6) continue;
      lookups++;
      const v = await this.env.indexer.order(id).catch(() => null);
      if (v) this.placed.delete(id);
    }
  }
}

