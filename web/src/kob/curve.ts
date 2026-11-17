// Stepped sell curve (`KobCurveAsk`, docs/design/curve-ask-spec.md): the default table (kob-wasm `curveTable`), the sale split of a
// creation ("Keep in my wallet" %), the form's validation, the graph model (price against the sold %, a step line) and the numbers shown
// beside it. The table arithmetic is Rust's; this file only maps text to the spec and the table to what the UI draws.
import { parseDecimal } from './issue';
import type { PlanIssue } from './plan-types';
import type { CurveAskState, I64 } from './types';
import { KobError, type KobWasm } from './wasm';

/** The UI defaults (spec §7): 100 levels, 10,000 KAS if sold out, last level 5x the first, keep 0%. */
export const CURVE_DEFAULTS = { levels: '100', totalKas: '10000', multiple: '5', keepPct: '0' } as const;
/** Gates: 1 to 1,000 levels. */
export const CURVE_MAX_LEVELS = 1000;
/** A fill touches at most 8 levels (spec §3). */
export const CURVE_MAX_CROSS = 8;

const SOMPI = 100_000_000n;

/** The requested curve (`defaults::CurveSpec`), base units and sompi. */
export interface CurveSpec {
  amount: bigint;
  levels: number;
  /** sompi if sold out */
  total: bigint;
  /** last level / first level, in thousandths (5x = 5000) */
  multipleMilli: bigint;
  /** base units per whole token */
  scale: bigint;
  tip?: bigint;
}

/** `defaults::curve_table`: base units per level, sompi per whole token per level, and what a sell-out pays. */
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
        }),
      ),
    ) as { levelSize: I64; table: I64[]; realised: I64 };
    return { levelSize: BigInt(o.levelSize), table: o.table.map((p) => BigInt(p)), realised: BigInt(o.realised) };
  });
}

const curveJson = (s: CurveAskState) => JSON.stringify({ kind: 'KobCurveAsk', state: s });

/** The most base units `budget` sompi buy from the curve in one fill (at most 8 levels). */
export function curveMaxForBudget(kob: KobWasm, s: CurveAskState, budget: bigint): bigint {
  return wrap('curveMaxForBudget', () => BigInt(rawCurve(kob).curveMaxForBudget(curveJson(s), budget.toString())));
}

/** The most one fill takes from here (the rest of 8 levels). */
export function curveMaxFill(kob: KobWasm, s: CurveAskState): bigint {
  return wrap('curveMaxFill', () => BigInt(rawCurve(kob).curveMaxFill(curveJson(s))));
}

/** The maker's proceeds (sompi) of a fill of `n`; null past 8 levels or the table. */
export function curveProceeds(kob: KobWasm, s: CurveAskState, n: bigint): bigint | null {
  return wrap('curveProceeds', () => {
    const v = rawCurve(kob).curveProceeds(curveJson(s), n.toString());
    return v === undefined ? null : BigInt(v);
  });
}

// ------------------------------------------------------------------------------------------------ levels and numbers

/** Base units of each level: `levelSize`, the last one what is left of `total`. */
export function levelSizes(levelSize: bigint, levels: number, total: bigint): bigint[] {
  return Array.from({ length: levels }, (_, i) => (i + 1 < levels ? levelSize : total - BigInt(levels - 1) * levelSize));
}

/** The current level of a state: (total − amountLeft) / levelSize. */
export function currentLevel(s: Pick<CurveAskState, 'total' | 'amountLeft' | 'levelSize'>): number {
  return Number((BigInt(s.total) - BigInt(s.amountLeft)) / BigInt(s.levelSize));
}

export interface CurveNumbers {
  levels: number;
  /** sompi per whole token */
  first: bigint;
  last: bigint;
  /** what a sell-out pays per whole token on average (rounded) */
  average: bigint;
  /** sompi if sold out (level by level, rounded up per level) */
  total: bigint;
}

/** First, last and average price and "Total if sold out" of a table over `amount` base units. */
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

// ------------------------------------------------------------------------------------------------ the graph

/** One step of the line: the share sold before and after the level (0..1), its price (sompi per whole token), sold already or not. */
export interface CurveStep {
  level: number;
  from: number;
  to: number;
  price: bigint;
  sold: boolean;
}

/**
 * The steps of a table over `total` base units; levels below `amountLeft` (sold) are marked. Many levels are merged into at most `maxSteps`
 * drawn steps only for display (each keeps its first level's price), so a 1,000-level curve stays a light SVG.
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

/** SVG geometry of a step line in a `w` x `h` box (price 0 at the bottom, the top level at `top` of the height). */
export interface StepGeometry {
  /** the whole line */
  path: string;
  /** the sold part (empty when nothing is sold) and the rest */
  soldPath: string;
  openPath: string;
  /** the filled area under the unsold part */
  area: string;
  /** x of the boundary between sold and unsold, null when nothing is sold */
  soldX: number | null;
  maxPrice: bigint;
}

const f2 = (x: number) => (Math.round(x * 100) / 100).toString();

export function stepGeometry(steps: readonly CurveStep[], w: number, h: number, top = 0.9): StepGeometry {
  const maxPrice = steps.reduce((m, s) => (s.price > m ? s.price : m), 0n);
  const y = (p: bigint) => (maxPrice > 0n ? h - (Number(p) / Number(maxPrice)) * h * top : h);
  const line = (xs: readonly CurveStep[]) => xs.map((s, i) => `${i === 0 ? 'M' : 'L'}${f2(s.from * w)} ${f2(y(s.price))} H${f2(s.to * w)}`).join(' ');
  const sold = steps.filter((s) => s.sold);
  const open = steps.filter((s) => !s.sold);
  const area = open.length ? `${line(open)} V${h} H${f2(open[0]!.from * w)} Z` : '';
  return { path: line(steps), soldPath: sold.length ? line(sold) : '', openPath: open.length ? line(open) : '', area, soldX: sold.length ? sold[sold.length - 1]!.to * w : null, maxPrice };
}

// ------------------------------------------------------------------------------------------------ the creation's sale

/** "Keep in my wallet" percent text -> basis points (0..10000, at most two decimals). */
export function parsePercent(text: string): { bps: bigint } | { error: 'syntax' | 'range' } {
  const p = parseDecimal(text.trim() === '' ? '0' : text, 2);
  if ('error' in p) return { error: 'syntax' };
  return p.value > 10_000n ? { error: 'range' } : { bps: p.value };
}

/** The share for sale and the share kept, from a keep percent of `share` (sale rounded down to a base unit). */
export function saleSplit(share: bigint, keepBps: bigint): { sell: bigint; keep: bigint } {
  const sell = (share * (10_000n - keepBps)) / 10_000n;
  return { sell, keep: share - sell };
}

/** The keep percent text (at most two decimals, trailing zeros dropped) of selling `sell` of `share`. */
export function keepPercentText(share: bigint, sell: bigint): string {
  if (share <= 0n) return '0';
  const keep = share - (sell < 0n ? 0n : sell > share ? share : sell);
  const bps = (keep * 10_000n + share / 2n) / share;
  const whole = bps / 100n;
  const frac = (bps % 100n).toString().padStart(2, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : whole.toString();
}

/** What the user types for a curve (and a one-price sale): text, so an emptied field is representable. */
export interface SaleForm {
  /** 'keep' | 'ask' (one KobAsk) | 'curve' */
  mode: 'keep' | 'ask' | 'curve';
  keepPct: string;
  /** whole tokens for sale (linked to keepPct) */
  amount: string;
  /** a curve */
  levels: string;
  /** KAS if sold out (a one-price sale: amount x price) */
  totalKas: string;
  /** last level / first level */
  multiple: string;
}

export function defaultSaleForm(): SaleForm {
  return { mode: 'keep', keepPct: CURVE_DEFAULTS.keepPct, amount: '', levels: CURVE_DEFAULTS.levels, totalKas: CURVE_DEFAULTS.totalKas, multiple: CURVE_DEFAULTS.multiple };
}

/** A sale form after parsing. */
export interface ParsedSale {
  mode: 'ask' | 'curve';
  /** base units for sale and kept */
  sell: bigint;
  keep: bigint;
  levels: number;
  /** sompi if sold out */
  total: bigint;
  multipleMilli: bigint;
}

const issue = (code: string, message: string, field: string, params?: PlanIssue['params']): PlanIssue => ({ code, severity: 'error', message, field: `sale.${field}`, ...(params ? { params } : {}) });

/**
 * Validates a sale of `share` base units (the wallet's share of the supply). `amount` wins over `keepPct` when both are set (the view keeps
 * them linked); an empty amount means `keepPct` of the share.
 */
export function parseSale(f: SaleForm, share: bigint | null, decimals: number): { issues: PlanIssue[]; parsed: ParsedSale | null } {
  const issues: PlanIssue[] = [];
  if (f.mode === 'keep') return { issues, parsed: null };
  let sell: bigint | null = null;
  if (f.amount.trim() !== '') {
    const a = parseDecimal(f.amount, decimals);
    if ('error' in a) issues.push(issue(a.error === 'precision' ? 'sale.amount.precision' : 'sale.amount.invalid', 'Enter the amount for sale as a number.', 'amount', { decimals }));
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
  const total = parseDecimal(f.totalKas, 8);
  if ('error' in total || total.value <= 0n) issues.push(issue('sale.total.invalid', 'Enter the KAS if sold out as a positive number.', 'totalKas'));
  let levels = 1;
  let multipleMilli = 1000n;
  if (f.mode === 'curve') {
    levels = /^\d{1,4}$/.test(f.levels.trim()) ? Number(f.levels.trim()) : Number.NaN;
    if (!(levels >= 1 && levels <= CURVE_MAX_LEVELS)) issues.push(issue('sale.levels.invalid', `Levels is a whole number from 1 to ${CURVE_MAX_LEVELS}.`, 'levels', { max: CURVE_MAX_LEVELS }));
    const m = parseDecimal(f.multiple, 3);
    if ('error' in m || m.value < 1000n) issues.push(issue('sale.multiple.invalid', 'The last level is at least the first (1 or more).', 'multiple'));
    else multipleMilli = m.value;
  }
  if (issues.length || sell === null || share === null || 'error' in total) return { issues, parsed: null };
  return { issues, parsed: { mode: f.mode, sell, keep: share - sell, levels, total: total.value, multipleMilli } };
}

/** KAS text of sompi. */
export const kasOf = (sompi: bigint): string => {
  const w = sompi / SOMPI;
  const fr = (sompi % SOMPI).toString().padStart(8, '0').replace(/0+$/, '');
  return fr ? `${w}.${fr}` : w.toString();
};
