// Test fixtures of the pair planner: two-token PairPlanEnvs (KCC-20 / KRON mixes) built on the shared planner fixtures (testing/fixtures.ts:
// the real kob-wasm node bindings, the maker key, UTXO factories), and an independent BigInt model of the covenants' maker-favour rounding that
// the tests check every disclosed amount against.
import type { PairBookView } from '../../data/indexer-types';
import type { BookView as PlanBook, Clock, OwnOrderRef, PairPlanEnv, TokenMarket } from '../plan-types';
import { CLOCK, FIXTURE_CARRIER, KAS, MAKER_PK, keyUtxo, kob, level, market3x3, market8x8, marketKron, tokenUtxo } from '../../testing/fixtures';

export { CLOCK, KAS, MAKER_PK, level };

/** Token A of the pairs: the 3x3 reference program, 3 decimals (scale 1000), covenant 70..70. */
export const A_COV = '70'.repeat(32);
/** Token B: the 8x8 program, 2 decimals (scale 100), covenant 71..71. */
export const B_COV = '71'.repeat(32);
export const KRON_A_COV = '72'.repeat(32);
export const KRON_B_COV = '73'.repeat(32);

export const tokenA = (): TokenMarket => market3x3({ ticker: 'AAA' });
export const tokenB = (): TokenMarket => market8x8({ covenant_id: B_COV, ticker: 'BBB', decimals: 2 });
/** KRON A: 3 decimals (scale 1000). */
export const kronA = (): TokenMarket => marketKron({ covenant_id: KRON_A_COV, ticker: 'KRA', decimals: 3 });
/** KRON B: 0 decimals (scale 1): one base unit per whole token. */
export const kronB = (): TokenMarket => marketKron({ covenant_id: KRON_B_COV, ticker: 'KRB', decimals: 0 });

/** Default pair book (B per whole A): best ask 1500, best bid 1450 (15.00 / 14.50 BBB per AAA). */
export function pairBook(): PlanBook {
  return {
    asks: [level(1500n, 5_000n, 2), level(1510n, 10_000n, 3), level(1600n, 20_000n, 1)],
    bids: [level(1450n, 5_000n, 1), level(1440n, 10_000n, 2), level(1400n, 20_000n, 3)],
  };
}

export interface PairEnvOptions {
  a?: TokenMarket;
  b?: TokenMarket;
  /** KAS funding UTXOs (sompi); default one of 100,000 KAS */
  funding?: bigint[];
  /** A UTXO amounts (base units); default one of 100 AAA */
  aAmounts?: bigint[];
  /** B UTXO amounts (base units); default one of 1,000,000 whole B */
  bAmounts?: bigint[];
  book?: PlanBook;
  view?: PairBookView | null;
  ownOrders?: OwnOrderRef[];
  /** sompi per whole A / B; default 3 KAS / 0.2 KAS (A is worth 15 B) */
  kasPerWholeA?: bigint | null;
  kasPerWholeB?: bigint | null;
  clock?: Clock;
  /** KAS per covenant UTXO; default `FIXTURE_CARRIER` (10 KAS), `null` = the wallet default */
  carrier?: bigint | null;
  lastFillPrice?: bigint | null;
  /** false: no further indexer answers (default: one that reports the book's best prices) */
  independentReference?: boolean;
}

/** A PairPlanEnv of `MAKER_PK` on the pair a/b (default AAA/BBB). */
export function makePairEnv(o: PairEnvOptions = {}): PairPlanEnv {
  const a = o.a ?? tokenA();
  const b = o.b ?? tokenB();
  const env: PairPlanEnv = {
    kob: kob(),
    token: a,
    maker: MAKER_PK,
    clock: o.clock ?? CLOCK,
    book: o.book ?? pairBook(),
    ownOrders: o.ownOrders ?? [],
    funding: (o.funding ?? [100_000n * KAS]).map((x) => keyUtxo(x)),
    tokenUtxos: (o.aAmounts ?? [100n * a.scale]).map((x) => tokenUtxo(a, x)),
    pair: {
      quote: b,
      quoteTokenUtxos: (o.bAmounts ?? [1_000_000n * b.scale]).map((x) => tokenUtxo(b, x)),
      view: o.view ?? null,
      kasPerWholeA: o.kasPerWholeA === undefined ? 3n * KAS : o.kasPerWholeA,
      kasPerWholeB: o.kasPerWholeB === undefined ? KAS / 5n : o.kasPerWholeB,
    },
  };
  const carrier = o.carrier === undefined ? FIXTURE_CARRIER : o.carrier;
  if (carrier !== null) env.carrier = carrier;
  if (o.lastFillPrice !== undefined) env.lastFillPrice = o.lastFillPrice;
  // a further indexer that reports the same best prices: an independent reference for the start of market / close orders
  if (o.independentReference !== false) {
    env.referenceTouches = [{ label: 'https://idx2.example', bestAsk: env.book.asks[0]?.price ?? null, bestBid: env.book.bids[0]?.price ?? null }];
  }
  return env;
}

// ------------------------------------------------------------------------------------------------ independent rounding model

/** what a maker RECEIVES: ceil(n * p / scale) */
export const ceilQ = (n: bigint, p: bigint, scale: bigint): bigint => (n * p + scale - 1n) / scale;
/** what a maker PAYS: floor(n * p / scale) */
export const floorQ = (n: bigint, p: bigint, scale: bigint): bigint => (n * p) / scale;
export const ceilDivM = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;
