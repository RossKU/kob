// Adapter of the indexer's pair API (docs/ops/executor.md 5.4, "Pair orders"): every pair view the client returns goes through here, so that
// nothing malformed reaches bigint math and every GUESS about a detail the document does not fix lives in this one file.
//
// Guesses (the document fixes the field names of each item but not every envelope; the executor of this branch serializes them as below):
//   * `GET /v1/pairs` is documented as a JSON array; a `{ items: [...] }` envelope is accepted too.
//   * `/candles` and `/fills` carry their rows in `items` (the 5.3 market-data convention); `next_cursor` of `/fills` may be absent (= null).
//   * `/fills` `volume_24h` may be absent on an older build: zeros are reported then.
//   * the counts of a `/v1/pairs` entry that an older build omits (`entry_asks`, `entry_bids`, `conditionals`) read as 0.
//   * event `detail.pair` / `detail.evidence` are read defensively (`pairDetailOf`, `evidenceDetailOf`): a field of the wrong type is dropped.
import type {
  EventView, PairBookView, PairCandleView, PairCandlesView, PairEventDetail, PairEvidenceDetail, PairFillView, PairFillsView, PairLevelView,
  PairRateView, PairSummaryView,
} from './indexer-types';

const DEC = /^\d{1,40}$/;
const HASH = /^[0-9a-f]{64}$/;
const SOURCES = new Set(['direct', 'entry', 'route']);
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const decOrNull = (v: unknown): string | null => (typeof v === 'string' && DEC.test(v) ? v : null);
const count = (v: unknown): number => (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : 0);

/** Keeps only well-formed pair levels (decimal-string price and amount, positive denominator, numerator and amount, a known source). */
export function sanitizePairBook(v: PairBookView): PairBookView {
  const ok = (l: PairLevelView): boolean =>
    isObj(l) && SOURCES.has(l.source) && typeof l.price_num === 'string' && typeof l.price_den === 'string' && typeof l.amount === 'string' &&
    DEC.test(l.price_num) && DEC.test(l.price_den) && DEC.test(l.amount) && BigInt(l.price_den) > 0n && BigInt(l.price_num) > 0n && BigInt(l.amount) > 0n;
  return { ...v, asks: (v.asks ?? []).filter(ok), bids: (v.bids ?? []).filter(ok) };
}

/** `GET /v1/pairs` answer (an array, or an `items` envelope) -> well-formed entries (oriented hex32 pairs, base != quote), counts defaulting to 0. */
export function adaptPairSummaries(raw: unknown): PairSummaryView[] | null {
  const items = Array.isArray(raw) ? raw : isObj(raw) && Array.isArray(raw.items) ? raw.items : null;
  if (!items) return null;
  const out: PairSummaryView[] = [];
  for (const p of items) {
    if (!isObj(p) || typeof p.base !== 'string' || typeof p.quote !== 'string' || !HASH.test(p.base) || !HASH.test(p.quote) || p.base === p.quote) continue;
    out.push({
      base: p.base, quote: p.quote, direct_asks: count(p.direct_asks), direct_bids: count(p.direct_bids), entry_asks: count(p.entry_asks),
      entry_bids: count(p.entry_bids), conditionals: count(p.conditionals),
    });
  }
  return out;
}

const rate = (v: unknown): PairRateView | null => {
  if (!isObj(v)) return null;
  const value = decOrNull(v.value);
  const num = decOrNull(v.num);
  const den = decOrNull(v.den);
  return value !== null && num !== null && den !== null && BigInt(den) > 0n ? { value, num, den } : null;
};

/** `/candles` answer -> the view with only well-formed candles (four valid rates, a numeric bucket time); null when it is not a candle view. */
export function adaptPairCandles(raw: unknown): PairCandlesView | null {
  if (!isObj(raw) || !Array.isArray(raw.items)) return null;
  const items: PairCandleView[] = [];
  for (const c of raw.items) {
    if (!isObj(c) || typeof c.t !== 'number') continue;
    const o = rate(c.o);
    const h = rate(c.h);
    const l = rate(c.l);
    const cl = rate(c.c);
    if (!o || !h || !l || !cl) continue;
    items.push({
      t: c.t, o, h, l, c: cl, a_traded: c.a_traded === true, b_traded: c.b_traded === true, pair_volume_a: decOrNull(c.pair_volume_a) ?? '0',
      pair_volume_b: decOrNull(c.pair_volume_b) ?? '0', pair_fills: count(c.pair_fills),
    });
  }
  const num = (v: unknown): number | null => (typeof v === 'number' && Number.isInteger(v) ? v : null);
  return {
    base: String(raw.base ?? ''), quote: String(raw.quote ?? ''), interval: raw.interval as PairCandlesView['interval'],
    price_basis: decOrNull(raw.price_basis) ?? '1', quote_price_basis: decOrNull(raw.quote_price_basis) ?? '1',
    decimals: num(raw.decimals), quote_decimals: num(raw.quote_decimals), price_source: typeof raw.price_source === 'string' ? raw.price_source : 'kas_books', items,
  };
}

/** `/fills` answer -> the view with only well-formed fills; `volume_24h` zeros and `next_cursor` null when absent. */
export function adaptPairFills(raw: unknown): PairFillsView | null {
  if (!isObj(raw) || !Array.isArray(raw.items)) return null;
  const items = raw.items.filter(
    (f): f is PairFillView => isObj(f) && typeof f.id === 'number' && typeof f.order === 'string' && (f.side === 'ask' || f.side === 'bid') && decOrNull(f.amount_a) !== null,
  );
  const v = isObj(raw.volume_24h) ? raw.volume_24h : {};
  return {
    base: String(raw.base ?? ''), quote: String(raw.quote ?? ''),
    volume_24h: { amount_a: decOrNull(v.amount_a) ?? '0', amount_b: decOrNull(v.amount_b) ?? '0', fills: count(v.fills) },
    items, next_cursor: cursorOf(raw.next_cursor),
  };
}

function cursorOf(v: unknown): string | null {
  if (typeof v === 'string' && v !== '') return v;
  return typeof v === 'number' && Number.isSafeInteger(v) ? String(v) : null;
}

/** `detail.pair` of a pair fill event (null for any other event). Pair fills never carry a KAS price: `price` here is the order's B quote. */
export function pairDetailOf(ev: Pick<EventView, 'detail'>): PairEventDetail | null {
  const d = isObj(ev.detail) ? ev.detail.pair : null;
  if (!isObj(d) || (d.side !== 'ask' && d.side !== 'bid') || typeof d.base !== 'string' || typeof d.quote !== 'string' || decOrNull(d.amount_a) === null) return null;
  return {
    side: d.side, base: d.base, quote: d.quote, a_scale: count(d.a_scale), amount_a: d.amount_a as string, amount_b: decOrNull(d.amount_b),
    price: decOrNull(d.price), price_num: decOrNull(d.price_num), price_den: decOrNull(d.price_den), tip_kas: decOrNull(d.tip_kas),
    counterparty: typeof d.counterparty === 'string' ? d.counterparty : 'inventory', price_source: 'none',
  };
}

/** `detail.evidence` of a pair conditional's arm / trail / triggered fill (mode 0: KAS quotes `a`, `b`; mode 1: a pair order's `price`). */
export function evidenceDetailOf(ev: Pick<EventView, 'detail'>): PairEvidenceDetail | null {
  const d = isObj(ev.detail) ? ev.detail.evidence : null;
  if (!isObj(d) || typeof d.mode !== 'number') return null;
  const out: PairEvidenceDetail = { mode: d.mode };
  if (Array.isArray(d.inputs)) out.inputs = d.inputs.filter((x): x is number => typeof x === 'number');
  if (Array.isArray(d.orders)) out.orders = d.orders.map((x) => (typeof x === 'string' ? x : null));
  const a = decOrNull(d.a);
  const b = decOrNull(d.b);
  const p = decOrNull(d.price);
  if (a !== null) out.a = a;
  if (b !== null) out.b = b;
  if (p !== null) out.price = p;
  return out;
}
