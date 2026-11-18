// The stepped sell curve (`KobCurveAsk`, docs/design/curve-ask-spec.md). This file holds:
// - the default table (kob-wasm `curveTable`),
// - the sale split of a creation ("Keep in my wallet" %),
// - the form's validation,
// - the graph model, a step line of price against the sold %,
// - the numbers shown beside it.
//
// The table arithmetic is Rust's. This file only maps text to the spec and the table to what the UI
// draws.
import { parseDecimal } from './issue';
import type { PlanIssue } from './plan-types';
import type { CurveAskState, I64 } from './types';
import { KobError, type KobWasm } from './wasm';

/**
 * The UI defaults (spec §7). They are 100 levels, 10,000 KAS if sold out, a last level 5x the
 * first and keep 0%. A chromatic curve spans 2 octaves.
 */
export const CURVE_DEFAULTS = { levels: '100', totalKas: '10000', multiple: '5', keepPct: '0', octaves: '2' } as const;
/** The most levels of a curve. A curve has 1 to 1,000. */
export const CURVE_MAX_LEVELS = 1000;
/**
 * The most octaves of a chromatic curve, which spans 1 to 8 (`defaults::CHROMATIC_MAX_OCTAVES`).
 * It has 12 x octaves + 1 levels.
 */
export const CHROMATIC_MAX_OCTAVES = 8;

/**
 * The table's shape (`defaults::CurveShape`).
 * - `linear` is geometric from the first level to `multiple` times it over `levels`.
 * - `chromatic` climbs one semitone (2^(1/12)) per level over `octaves` octaves, so every 12
 *   levels exactly double.
 */
export type CurveShape = 'linear' | 'chromatic';
/** A fill touches at most 8 levels (spec §3). */
export const CURVE_MAX_CROSS = 8;

const SOMPI = 100_000_000n;

/** The requested curve (`defaults::CurveSpec`), in base units and sompi. */
export interface CurveSpec {
  amount: bigint;
  levels: number;
  /** Sompi if sold out. */
  total: bigint;
  /** The last level over the first, in thousandths, so 5x is 5000. */
  multipleMilli: bigint;
  /** Base units per whole token. */
  scale: bigint;
  tip?: bigint;
  /**
   * The shape, linear by default. A chromatic curve takes `octaves` (1 to 8) and does not use
   * `levels` and `multipleMilli`.
   */
  shape?: CurveShape;
  octaves?: number;
}

/**
 * The table of `defaults::curve_table`. It holds the base units per level, the sompi per whole
 * token of each level, and what a sell-out pays.
 */
export interface CurveTable {
  levelSize: bigint;
  table: bigint[];
  realised: bigint;
}

interface RawCurve {
  curveTable(spec: string): string;
  curveMaxForBudget(state: string, budget: string): string;
  curveMaxFill(state: string): string;
  curveProceeds(state: string, n: string): string | undefined;
}

function rawCurve(kob: KobWasm): RawCurve {
  const r = kob.raw as unknown as Partial<RawCurve>;
  if (typeof r.curveTable !== 'function') throw new KobError('the loaded kob-wasm bindings have no curve exports: rebuild them (npm run build:wasm)', 'curveTable');
  return r as RawCurve;
}

const wrap = <T>(op: string, f: () => T): T => {
  try {
    return f();
  } catch (e) {
    throw e instanceof KobError ? e : new KobError(e instanceof Error ? e.message : String(e), op);
  }
};

/** The default table of `spec` (spec §7), from kob-wasm. */
export function curveTable(kob: KobWasm, spec: CurveSpec): CurveTable {
  return wrap('curveTable', () => {
    const o = JSON.parse(
      rawCurve(kob).curveTable(
        JSON.stringify({
          amount: spec.amount.toString(),
          levels: spec.levels,
          total: spec.total.toString(),
          multipleMilli: spec.multipleMilli.toString(),
          scale: spec.scale.toString(),
          tip: (spec.tip ?? 0n).toString(),
          ...(spec.shape === 'chromatic' ? { shape: 'chromatic', octaves: spec.octaves ?? 0 } : {}),
        }),
      ),
    ) as { levelSize: I64; table: I64[]; realised: I64 };
    return { levelSize: BigInt(o.levelSize), table: o.table.map((p) => BigInt(p)), realised: BigInt(o.realised) };
  });
}

const curveJson = (s: CurveAskState) => JSON.stringify({ kind: 'KobCurveAsk', state: s });

/**
 * The most base units `budget` sompi buy from the curve in one fill, which crosses at most 8
 * levels.
 */
export function curveMaxForBudget(kob: KobWasm, s: CurveAskState, budget: bigint): bigint {
  return wrap('curveMaxForBudget', () => BigInt(rawCurve(kob).curveMaxForBudget(curveJson(s), budget.toString())));
}

/** The most one fill takes from here, which is the rest of 8 levels. */
export function curveMaxFill(kob: KobWasm, s: CurveAskState): bigint {
  return wrap('curveMaxFill', () => BigInt(rawCurve(kob).curveMaxFill(curveJson(s))));
}

/** The maker's proceeds in sompi of a fill of `n`. It is null past 8 levels or past the table. */
export function curveProceeds(kob: KobWasm, s: CurveAskState, n: bigint): bigint | null {
  return wrap('curveProceeds', () => {
    const v = rawCurve(kob).curveProceeds(curveJson(s), n.toString());
    return v === undefined ? null : BigInt(v);
  });
}

// --- Levels and numbers ---

/** The base units of each level. Each is `levelSize`, and the last is what is left of `total`. */
export function levelSizes(levelSize: bigint, levels: number, total: bigint): bigint[] {
  return Array.from({ length: levels }, (_, i) => (i + 1 < levels ? levelSize : total - BigInt(levels - 1) * levelSize));
}

/**
 * The levels left of a curve holding `left` base units, the current one first. Each gives its
 * table index, its price and the size left in it.
 */
export function curveLevelsLeft(levelSize: bigint, table: readonly bigint[], total: bigint, left: bigint): { level: number; price: bigint; amount: bigint }[] {
  const out: { level: number; price: bigint; amount: bigint }[] = [];
  const sold = total - left;
  let pos = 0n;
  levelSizes(levelSize, table.length, total).forEach((size, j) => {
    const end = pos + size;
    const open = end > sold ? end - (sold > pos ? sold : pos) : 0n;
    if (open > 0n) out.push({ level: j, price: table[j]!, amount: open });
    pos = end;
  });
  return out;
}

/**
 * The sompi the levels left pay if they all sell. Each level's open part counts at its price,
 * rounded up per level, which is what the covenant pays the maker.
 */
export function curveRestKas(levelSize: bigint, table: readonly bigint[], total: bigint, left: bigint, scale: bigint): bigint {
  return curveLevelsLeft(levelSize, table, total, left).reduce((a, l) => a + (scale > 0n ? (l.amount * l.price + scale - 1n) / scale : 0n), 0n);
}

/**
 * The first, last and average price and "Total if sold out" of a curve's whole table, from its
 * state.
 */
export function curveNumbersOfState(s: Pick<CurveAskState, 'levelSize' | 'table' | 'total' | 'scale'>): CurveNumbers {
  const table = s.table.map((p) => BigInt(p));
  const total = BigInt(s.total);
  const scale = BigInt(s.scale);
  return curveNumbers({ levelSize: BigInt(s.levelSize), table, realised: curveRestKas(BigInt(s.levelSize), table, total, total, scale) }, total, scale);
}

/**
 * The table of an amended curve. Per spec §5, the amend keeps `total` and `amountLeft`, and may
 * change `levelSize` and the table.
 *
 * The levels already sold take the new current price, since nothing fills them again. The rest
 * rise geometrically from `now` to `last` in whole sompi and never fall. It returns null when the
 * level size gives more than 1,000 levels or a price is not positive.
 */
export function amendCurveTable(a: { total: bigint; amountLeft: bigint; levelSize: bigint; now: bigint; last: bigint }): { levelSize: bigint; table: bigint[] } | null {
  const { total, amountLeft, levelSize, now, last } = a;
  if (levelSize <= 0n || now <= 0n || last < now || amountLeft <= 0n || amountLeft > total) return null;
  const n = Number((total + levelSize - 1n) / levelSize);
  if (n < 1 || n > CURVE_MAX_LEVELS) return null;
  const cur = Number((total - amountLeft) / levelSize);
  const steps = n - 1 - cur;
  const r = steps > 0 ? Math.pow(Number(last) / Number(now), 1 / steps) : 1;
  const table: bigint[] = [];
  let prev = now;
  for (let j = 0; j < n; j++) {
    const k = j - cur;
    let p = k <= 0 ? now : j === n - 1 ? last : BigInt(Math.round(Number(now) * Math.pow(r, k)));
    if (p < prev) p = prev;
    if (p > last) p = last;
    table.push(p);
    prev = p;
  }
  return { levelSize, table };
}

/**
 * What the wallet refuses in a curve's state, as the `CurveProblem` codes `curveProblems`
 * returns. It is stricter than the protocol, which only asks that every price is at least the tip
 * and above 0. It refuses:
 * - a table that is empty, too long, has a price that is not positive, or falls anywhere,
 * - a tip at or above any level's price,
 * - a level size or total the table cannot hold,
 * - an amount left above the total,
 * - no minimum fill, or one larger than a fill can reach,
 * - with the token's decimals, a scale other than 10^min(decimals, 9),
 * - a negative start.
 *
 * It returns an empty list when the curve is sound.
 */
export type CurveProblem = 'table' | 'falls' | 'tip' | 'levelSize' | 'total' | 'amountLeft' | 'minFill' | 'scale' | 'activeFrom';
/** Lists the problems of a curve's state, as the type above describes them. */
export function curveProblems(s: Pick<CurveAskState, 'table' | 'tip' | 'levelSize' | 'total' | 'amountLeft' | 'minFill' | 'scale' | 'activeFrom'>, decimals?: number): CurveProblem[] {
  const out: CurveProblem[] = [];
  const num = (v: unknown): bigint | null => {
    try {
      return typeof v === 'string' && /^-?\d+$/.test(v) ? BigInt(v) : null;
    } catch {
      return null;
    }
  };
  const table = Array.isArray(s.table) ? s.table.map(num) : [];
  const tip = num(s.tip);
  const size = num(s.levelSize);
  const total = num(s.total);
  const left = num(s.amountLeft);
  const minFill = num(s.minFill);
  const scale = num(s.scale);
  const n = BigInt(table.length);
  if (!table.length || table.length > CURVE_MAX_LEVELS || table.some((p) => p === null || p <= 0n)) out.push('table');
  else if (table.some((p, i) => i > 0 && p! < table[i - 1]!)) out.push('falls');
  if (tip === null || tip < 0n || (table.length && table.every((p) => p !== null) && table.some((p) => tip >= p!))) out.push('tip');
  if (size === null || size <= 0n) out.push('levelSize');
  else if (total === null || total <= 0n || total > size * n || total <= size * (n - 1n)) out.push('total');
  if (left === null || left <= 0n || (total !== null && left > total)) out.push('amountLeft');
  // One fill crosses at most 8 levels. From any position it reaches 7 levels + 1 base unit, so a
  // larger minimum can leave the curve unfillable.
  if (minFill === null || minFill < 1n || (size !== null && size > 0n && minFill > (BigInt(CURVE_MAX_CROSS) - 1n) * size + 1n)) out.push('minFill');
  if (scale === null || scale <= 0n || (decimals !== undefined && scale !== 10n ** BigInt(Math.min(decimals, 9)))) out.push('scale');
  const af = num(s.activeFrom);
  if (af === null || af < 0n) out.push('activeFrom');
  return out;
}

/**
 * The default minimum fill of a curve. It is the amount worth 10 KAS at its first level (kob-wasm
 * `defaultMinFill`), at most one level.
 */
export function curveMinFill(kob: KobWasm, amount: bigint, firstPrice: bigint, levelSize: bigint, scale: bigint): bigint {
  const m = kob.defaultMinFill(amount, firstPrice, scale);
  return m < levelSize ? m : levelSize > 0n ? levelSize : 1n;
}

/**
 * A short preview of a table, the first three levels and then the last two, as in
 * "1 · 2 · 3 … 9 · 10". A table of up to 6 levels is shown whole. `fmt` prints one price.
 */
export function tablePreview(table: readonly bigint[], fmt: (p: bigint) => string): string {
  if (table.length <= 6) return table.map(fmt).join(' · ');
  return `${table.slice(0, 3).map(fmt).join(' · ')} … ${table.slice(-2).map(fmt).join(' · ')}`;
}

/** The current level of a state, (total − amountLeft) / levelSize. */
export function currentLevel(s: Pick<CurveAskState, 'total' | 'amountLeft' | 'levelSize'>): number {
  return Number((BigInt(s.total) - BigInt(s.amountLeft)) / BigInt(s.levelSize));
}

/** The numbers shown beside a curve. */
export interface CurveNumbers {
  levels: number;
  /** Sompi per whole token. */
  first: bigint;
  last: bigint;
  /** What a sell-out pays per whole token on average, rounded. */
  average: bigint;
  /** Sompi if sold out, level by level and rounded up per level. */
  total: bigint;
}

/**
 * The first, last and average price and "Total if sold out" of a table over `amount` base units.
 */
export function curveNumbers(t: CurveTable, amount: bigint, scale: bigint): CurveNumbers {
  const n = t.table.length;
  return {
    levels: n,
    first: t.table[0] ?? 0n,
    last: t.table[n - 1] ?? 0n,
    average: amount > 0n ? (t.realised * scale + amount / 2n) / amount : 0n,
    total: t.realised,
  };
}

// --- The graph ---

/**
 * One step of the line. It gives the share sold before and after the level (0..1), its price in
 * sompi per whole token, and whether it is sold already.
 */
export interface CurveStep {
  level: number;
  from: number;
  to: number;
  price: bigint;
  sold: boolean;
}

/**
 * The steps of a table over `total` base units, one per level. A level whose whole size is sold,
 * given `amountLeft`, is marked as sold.
 */
export function curveSteps(levelSize: bigint, table: readonly bigint[], total: bigint, amountLeft: bigint = total): CurveStep[] {
  const sizes = levelSizes(levelSize, table.length, total);
  const soldUnits = total - amountLeft;
  const out: CurveStep[] = [];
  let acc = 0n;
  const share = (u: bigint) => (total > 0n ? Number((u * 1_000_000n) / total) / 1_000_000 : 0);
  table.forEach((price, level) => {
    const from = acc;
    acc += sizes[level]!;
    out.push({ level, from: share(from), to: share(acc), price, sold: acc <= soldUnits });
  });
  return out;
}

/**
 * The SVG geometry of a step line in a `w` x `h` box. Price 0 is at the bottom, and the top level
 * sits at `top` of the height.
 */
export interface StepGeometry {
  /** The whole line. */
  path: string;
  /** The sold part, empty when nothing is sold, and the rest. */
  soldPath: string;
  openPath: string;
  /** The filled area under the unsold part. */
  area: string;
  /** The x of the boundary between sold and unsold, or null when nothing is sold. */
  soldX: number | null;
  maxPrice: bigint;
}

const f2 = (x: number) => (Math.round(x * 100) / 100).toString();

/** Draws `steps` as a step line in a `w` x `h` box. */
export function stepGeometry(steps: readonly CurveStep[], w: number, h: number, top = 0.9): StepGeometry {
  const maxPrice = steps.reduce((m, s) => (s.price > m ? s.price : m), 0n);
  const y = (p: bigint) => (maxPrice > 0n ? h - (Number(p) / Number(maxPrice)) * h * top : h);
  const line = (xs: readonly CurveStep[]) => xs.map((s, i) => `${i === 0 ? 'M' : 'L'}${f2(s.from * w)} ${f2(y(s.price))} H${f2(s.to * w)}`).join(' ');
  const sold = steps.filter((s) => s.sold);
  const open = steps.filter((s) => !s.sold);
  const area = open.length ? `${line(open)} V${h} H${f2(open[0]!.from * w)} Z` : '';
  return { path: line(steps), soldPath: sold.length ? line(sold) : '', openPath: open.length ? line(open) : '', area, soldX: sold.length ? sold[sold.length - 1]!.to * w : null, maxPrice };
}

// --- The creation's sale ---

/**
 * Parses the "Keep in my wallet" percent text into basis points, 0..10000 with at most two
 * decimals.
 */
export function parsePercent(text: string): { bps: bigint } | { error: 'syntax' | 'range' } {
  const p = parseDecimal(text.trim() === '' ? '0' : text, 2);
  if ('error' in p) return { error: 'syntax' };
  return p.value > 10_000n ? { error: 'range' } : { bps: p.value };
}

/**
 * The share for sale and the share kept, from a keep percent of `share`. The sale rounds down to a
 * base unit.
 */
export function saleSplit(share: bigint, keepBps: bigint): { sell: bigint; keep: bigint } {
  const sell = (share * (10_000n - keepBps)) / 10_000n;
  return { sell, keep: share - sell };
}

/**
 * The keep percent text of selling `sell` of `share`, with at most two decimals and trailing zeros
 * dropped.
 */
export function keepPercentText(share: bigint, sell: bigint): string {
  if (share <= 0n) return '0';
  const keep = share - (sell < 0n ? 0n : sell > share ? share : sell);
  const bps = (keep * 10_000n + share / 2n) / share;
  const whole = bps / 100n;
  const frac = (bps % 100n).toString().padStart(2, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : whole.toString();
}

/**
 * What the user types for a curve or a one-price sale. It is text, so an emptied field is
 * representable.
 */
export interface SaleForm {
  /** `keep`, `ask` for one KobAsk, or `curve`. */
  mode: 'keep' | 'ask' | 'curve';
  keepPct: string;
  /** Whole tokens for sale, linked to keepPct. */
  amount: string;
  /** For a curve. */
  levels: string;
  /** KAS if sold out. For a one-price sale it is amount x price. */
  totalKas: string;
  /** The last level over the first. */
  multiple: string;
  /**
   * A curve's shape. A chromatic one takes `octaves` (1 to 8) in place of levels and multiple.
   */
  shape: CurveShape;
  octaves: string;
  /** For a one-price sale, KAS per whole token. */
  price: string;
}

/** The sale form with the UI defaults, set to keep everything. */
export function defaultSaleForm(): SaleForm {
  return { mode: 'keep', keepPct: CURVE_DEFAULTS.keepPct, amount: '', levels: CURVE_DEFAULTS.levels, totalKas: CURVE_DEFAULTS.totalKas, multiple: CURVE_DEFAULTS.multiple, shape: 'linear', octaves: CURVE_DEFAULTS.octaves, price: '' };
}

/** A sale form after parsing. */
export interface ParsedSale {
  mode: 'ask' | 'curve';
  /** Base units for sale and kept. */
  sell: bigint;
  keep: bigint;
  /** For a chromatic curve it is 12 x octaves + 1. */
  levels: number;
  /** Sompi if sold out. */
  total: bigint;
  multipleMilli: bigint;
  shape: CurveShape;
  /** A chromatic curve's octaves, or 0 otherwise. */
  octaves: number;
  /** For a one-price sale, sompi per whole token. */
  price: bigint;
}

/** The curve spec of a parsed curve sale at `scale`. */
export function curveSpecOfSale(p: ParsedSale, scale: bigint): CurveSpec {
  return { amount: p.sell, levels: p.levels, total: p.total, multipleMilli: p.multipleMilli, scale, ...(p.shape === 'chromatic' ? { shape: 'chromatic', octaves: p.octaves } : {}) };
}

const issue = (code: string, message: string, field: string, params?: PlanIssue['params']): PlanIssue => ({ code, severity: 'error', message, field: `sale.${field}`, ...(params ? { params } : {}) });

/**
 * Validates a sale of `share` base units, the wallet's share of the supply. `amount` wins over
 * `keepPct` when both are set, because the view keeps them linked. An empty amount means `keepPct`
 * of the share.
 */
export function parseSale(f: SaleForm, share: bigint | null, decimals: number): { issues: PlanIssue[]; parsed: ParsedSale | null } {
  const issues: PlanIssue[] = [];
  if (f.mode === 'keep') return { issues, parsed: null };
  let sell: bigint | null = null;
  if (f.amount.trim() !== '') {
    const a = parseDecimal(f.amount, decimals);
    if ('error' in a && a.error === 'precision') issues.push(issue('sale.amount.precision', `The amount for sale has more than ${decimals} decimal places.`, 'amount', { decimals }));
    else if ('error' in a) issues.push(issue('sale.amount.invalid', 'Enter the amount for sale as a number.', 'amount'));
    else sell = a.value;
  } else {
    const k = parsePercent(f.keepPct);
    if ('error' in k) issues.push(issue('sale.keep.invalid', 'Keep is a percent from 0 to 100.', 'keepPct'));
    else if (share !== null) sell = saleSplit(share, k.bps).sell;
  }
  if (sell !== null && share !== null) {
    if (sell <= 0n) issues.push(issue('sale.amount.zero', 'Nothing is left for sale: lower the share you keep.', 'amount'));
    else if (sell > share) issues.push(issue('sale.amount.too_large', 'The amount for sale is more than your share of the supply.', 'amount'));
  }
  const total = f.mode === 'curve' ? parseDecimal(f.totalKas, 8) : ({ value: 1n } as const);
  if ('error' in total || total.value <= 0n) issues.push(issue('sale.total.invalid', 'Enter the KAS if sold out as a positive number.', 'totalKas'));
  let price = 0n;
  if (f.mode === 'ask') {
    const p = parseDecimal(f.price, 8);
    if ('error' in p || p.value <= 0n) issues.push(issue('sale.price.invalid', 'Enter the price per token in KAS.', 'price'));
    else price = p.value;
  }
  let levels = 1;
  let multipleMilli = 1000n;
  const shape: CurveShape = f.mode === 'curve' && f.shape === 'chromatic' ? 'chromatic' : 'linear';
  let octaves = 0;
  if (shape === 'chromatic') {
    octaves = /^\d$/.test(f.octaves.trim()) ? Number(f.octaves.trim()) : Number.NaN;
    if (!(octaves >= 1 && octaves <= CHROMATIC_MAX_OCTAVES)) issues.push(issue('sale.octaves.invalid', `Octaves is a whole number from 1 to ${CHROMATIC_MAX_OCTAVES}.`, 'octaves', { max: CHROMATIC_MAX_OCTAVES }));
    else levels = 12 * octaves + 1;
  } else if (f.mode === 'curve') {
    levels = /^\d{1,4}$/.test(f.levels.trim()) ? Number(f.levels.trim()) : Number.NaN;
    if (!(levels >= 1 && levels <= CURVE_MAX_LEVELS)) issues.push(issue('sale.levels.invalid', `Levels is a whole number from 1 to ${CURVE_MAX_LEVELS}.`, 'levels', { max: CURVE_MAX_LEVELS }));
    const m = parseDecimal(f.multiple, 3);
    if ('error' in m || m.value < 1000n) issues.push(issue('sale.multiple.invalid', 'The last level is at least the first (1 or more).', 'multiple'));
    else multipleMilli = m.value;
  }
  if (issues.length || sell === null || share === null || 'error' in total) return { issues, parsed: null };
  const sold = f.mode === 'ask' ? (sell * price + (10n ** BigInt(decimals) - 1n)) / 10n ** BigInt(decimals) : total.value;
  return { issues, parsed: { mode: f.mode, sell, keep: share - sell, levels, total: sold, multipleMilli, shape, octaves, price } };
}

/**
 * The default one price of a sale, 10,000 KAS over the amount. It is in sompi per whole token and
 * at least 1.
 */
export function defaultSalePrice(sell: bigint, scale: bigint): bigint {
  if (sell <= 0n) return 1n;
  const p = (BigInt(CURVE_DEFAULTS.totalKas) * SOMPI * scale) / sell;
  return p > 0n ? p : 1n;
}

/** Formats sompi as KAS text. */
export const kasOf = (sompi: bigint): string => {
  const w = sompi / SOMPI;
  const fr = (sompi % SOMPI).toString().padStart(8, '0').replace(/0+$/, '');
  return fr ? `${w}.${fr}` : w.toString();
};
