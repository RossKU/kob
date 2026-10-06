// Pure invariant rules of the soak checker (no I/O, no `@/` imports: `node --test test/*.test.ts` runs them directly).
//
// Protocol v3 has no lots: every quantity (`n`, `amountLeft`, `minFill`, `minTouch`, custody) is token BASE UNITS; every price and tip is
// sompi per WHOLE token, i.e. per `scale` base units (`scale = 10^decimals`, a state field of the order). The value of n base units at the
// rate r is `quoteOf(n, r) = n * r / scale`, rounded in the maker's favour (BigInt: exact, no 64-bit limit). The formulas are those of
// crates/kob-protocol/src/state.rs and contracts/v2/*.sil, docs/spec/matcher.md §2.1 and docs/spec/order-types.md "Amounts, prices and rounding":
//
//   sell (ask side) receives at least  ceil(n * (q - tip) / scale)      q = the order's quote at the fill's time argument t
//   buy  (bid side) pays at most       floor(n * (q + tip) / scale)
//
// and the quote q never lies beyond the order's WORST bound:
//
//   KobAsk      q >= price, or >= priceEnd for a decaying ask (slope > 0: q = max(priceEnd, price - slope*steps))
//   KobBid      q <= price, or <= priceEnd for a rising bid (slope > 0, priceEnd > price: q = min(priceEnd, price + ...))
//   KobCondAsk  q >= min(tpPrice [if > 0], stopFloor [if stopPrice > 0]),  stopFloor   = stop - floor(stop / 10^4) * slipBps
//   KobCondBid  q <= max(tpPrice,          stopCeiling [if stopPrice > 0]), stopCeiling = stop + floor(stop / 10^4) * slipBps
//               (a stop-limit is a stop whose band ends at the limit; a trailing stop moves its band, so an older fill may lie
//               beyond the CURRENT band: trailing orders get the payout check only)
//   KobIfdBid   q <= price (a stop entry auctions from entryStop <= price up to price)
//   KobIfdAsk   q >= price (a stop entry auctions from entryStop >= price down to price)
//
// The fill event (`GET /v1/orders/{id}/events`, `GET /v1/fills`) carries `price` = the covenant's quote at the call's `t` (computed from
// the spent state when `detail.verified`) and, for side-1 fills, `payout` = the value of the output at the order input's index. For
// `KobAsk` and `KobCondAsk` that output is P2PK(maker) and the covenant requires it to hold at least the all-in proceeds (plus carriers on
// a sell-out), so `payout >= ceil(n * (q - tip) / scale)`; a booked repeat exit's merged take-profit pays the maker only the profit:
// `payout >= ceil(n * (q - tip) / scale) - ceil(n * rptPrice / scale)`. A `KobIfdAsk` entry fill pays the proceeds into its new exit instead:
// `exit value >= ceil(n * (q - tip) / scale)` (+ prefund and carriers). What a BID pays is not exposed by the read API (no payout for
// side 2, no order values per event): buy fills are checked by the quote bound only.
import { INV, type EventLike, type HealthLike, type OrderLike, type Violation } from './types.ts';

export const big = (v: unknown): bigint | null => {
  if (v === null || v === undefined || v === '') return null;
  try {
    return BigInt(v as string | number | bigint);
  } catch {
    return null;
  }
};
const b0 = (v: unknown): bigint => big(v) ?? 0n;
const maxB = (a: bigint, b: bigint) => (a > b ? a : b);
const minB = (a: bigint, b: bigint) => (a < b ? a : b);
/**
 * `quoteOf(n, rate, scale)`: the value of n base units at `rate` sompi (or token B base units) per whole token = per `scale` base units,
 * rounded down (`'down'`: what a maker pays) or up (`'up'`: what a maker must receive). Exact in BigInt; the covenants' split
 * multiplication equals it whenever the result fits 63 bits (the builders refuse orders beyond 2^62). null: a negative input or no scale.
 */
export function quoteOf(n: bigint, rate: bigint, scale: bigint, round: 'down' | 'up'): bigint | null {
  if (n < 0n || rate < 0n || scale <= 0n) return null;
  return (n * rate + (round === 'up' ? scale - 1n : 0n)) / scale;
}
const str = (v: unknown) => (typeof v === 'bigint' ? v.toString() : v);
const detailOf = (e: EventLike): Record<string, unknown> => (e.detail && typeof e.detail === 'object' ? (e.detail as Record<string, unknown>) : {});

/** the KCC-20 base kind (`KobAskKron` -> `KobAsk`) */
export const baseKind = (contract: string): string => contract.replace(/Kron$/, '');
export const isAskSide = (contract: string): boolean => ['KobAsk', 'KobCondAsk', 'KobIfdAsk'].includes(baseKind(contract));
export const isActive = (status: string): boolean => status === 'open' || status === 'partial';

// ------------------------------------------------------------------------------------------------ 3. all-in limits per fill

export interface WorstBound {
  /** `min`: the quote must be >= bound (sell); `max`: <= bound (buy) */
  dir: 'min' | 'max';
  bound: bigint | null;
  skip?: string;
}

// the covenant's band multiplies first (`stopPrice * slipBps / 10000`, exact to one sompi at any stop price): dividing first lost up to
// slipBps sompi once v3 prices (per whole token) stopped being multiples of 10000, and flagged fills at the true ceiling (TN10 2026-10-06)
export function stopFloor(s: Record<string, string>): bigint {
  const stop = b0(s.stopPrice);
  return stop - (stop * b0(s.slipBps)) / 10_000n;
}
export function stopCeiling(s: Record<string, string>): bigint {
  const stop = b0(s.stopPrice);
  return stop + (stop * b0(s.slipBps)) / 10_000n;
}

/** the worst quote an order can ever fill at (see the header) */
export function worstBound(contract: string, s: Record<string, string>): WorstBound {
  switch (baseKind(contract)) {
    case 'KobAsk':
      return { dir: 'min', bound: b0(s.slope) !== 0n ? b0(s.priceEnd) : b0(s.price) };
    case 'KobBid': {
      const p = b0(s.price);
      const e = b0(s.priceEnd);
      return { dir: 'max', bound: b0(s.slope) !== 0n && e > p ? e : p };
    }
    case 'KobCondAsk': {
      if (b0(s.trailStep) > 0n && b0(s.stopPrice) > 0n) return { dir: 'min', bound: null, skip: 'trailing stop (band moves)' };
      const c: bigint[] = [];
      if (b0(s.tpPrice) > 0n) c.push(b0(s.tpPrice));
      if (b0(s.stopPrice) > 0n) c.push(stopFloor(s));
      return { dir: 'min', bound: c.length ? c.reduce(minB) : null };
    }
    case 'KobCondBid': {
      if (b0(s.trailStep) > 0n && b0(s.stopPrice) > 0n) return { dir: 'max', bound: null, skip: 'trailing stop (band moves)' };
      const c: bigint[] = [];
      if (b0(s.tpPrice) > 0n) c.push(b0(s.tpPrice));
      if (b0(s.stopPrice) > 0n) c.push(stopCeiling(s));
      return { dir: 'max', bound: c.length ? c.reduce(maxB) : null };
    }
    case 'KobIfdBid':
      return { dir: 'max', bound: b0(s.price) };
    case 'KobIfdAsk':
      return { dir: 'min', bound: b0(s.price) };
    default:
      return { dir: 'min', bound: null, skip: `unknown contract ${contract}` };
  }
}

/**
 * The terms an order filled under, given its in-place amends (protocol v2.7 / KOB1 v3: a plain ask or bid amended in place keeps its covenant id;
 * the indexer records an `amend` event whose `detail.previous` holds the terms BEFORE it: price, tif, expiryDaa, activeFrom, tip).
 * The terms at a fill are the `previous` of the first amend after it (event ids only grow), else the order's current state.
 */
export function termsAtFill(memoTerms: Record<string, string>, current: Record<string, string> | null, events: EventLike[], fillId: number): Record<string, string> {
  const amends = events.filter((e) => e.kind === 'amend').sort((a, b) => Number(a.id ?? 0) - Number(b.id ?? 0));
  if (!amends.length) return memoTerms;
  const next = amends.find((e) => Number(e.id ?? 0) > fillId);
  if (!next) return current ? { ...memoTerms, ...current } : memoTerms;
  const prev = (detailOf(next).previous ?? {}) as Record<string, unknown>;
  const out: Record<string, string> = { ...memoTerms };
  for (const [k, v] of Object.entries(prev)) if (v !== null && v !== undefined) out[k] = String(v);
  return out;
}

export interface FillCheck {
  violations: Violation[];
  boundChecked: boolean;
  payoutChecked: boolean;
}

/**
 * Invariant 3 for one fill event of an order (`s` = the order's proven state; the immutable terms are all this rule reads, except the stop
 * of a trailing order, which it skips). `rptPrice` of a booked exit is immutable too.
 */
export function checkFill(order: { covenant_id: string; contract: string }, s: Record<string, string>, ev: EventLike): FillCheck {
  const out: FillCheck = { violations: [], boundChecked: false, payoutChecked: false };
  // a pair order's fill is in B, not KAS: invariant 11 (`checkPairFill`)
  if (ev.kind !== 'fill' || isPairContract(order.contract)) return out;
  const d = detailOf(ev);
  const n = big(ev.amount);
  const q = big(ev.price);
  const scale = b0(s.scale);
  const tip = b0(s.tip);
  // value of n base units at a rate (null when the terms carry no scale: a state of an older layout, not checkable)
  const quote = (n0: bigint, r: bigint, round: 'down' | 'up') => quoteOf(n0, r, scale, round);
  const net = (p: bigint) => (p > tip ? p - tip : 0n);
  // terms rebuilt from a terminated order's view (no proven state left): only a bid's cap is known exactly (from budget_rate)
  const partial = s._partial === '1';
  const wb: WorstBound = partial && baseKind(order.contract) !== 'KobBid' ? { dir: 'min', bound: null, skip: 'terms of a terminated order' } : worstBound(order.contract, s);
  const subject = `${order.covenant_id}:${ev.txid}`;
  const base = { order: order.covenant_id, contract: order.contract, txid: ev.txid, daa: ev.daa, amount: ev.amount, price: ev.price, payout: ev.payout };
  if (n !== null && n <= 0n) {
    out.violations.push({ invariant: INV.allIn, severity: 'error', subject, detail: { ...base, problem: 'fill of zero or negative amount' } });
  }
  if (q !== null && wb.bound !== null) {
    out.boundChecked = true;
    const beyond = wb.dir === 'min' ? q < wb.bound : q > wb.bound;
    if (beyond) {
      out.violations.push({
        invariant: INV.allIn,
        severity: 'error',
        subject,
        detail: { ...base, problem: `fill price beyond the order's worst bound (${wb.dir === 'min' ? 'sell below' : 'buy above'})`, worst: str(wb.bound) },
      });
    }
  }
  const kind = baseKind(order.contract);
  const payout = big(ev.payout);
  if (n !== null && n > 0n && payout !== null && ev.side === 1 && (kind === 'KobAsk' || kind === 'KobCondAsk')) {
    const verified = d.verified === true && d.t !== undefined && d.t !== null;
    const qt = verified && q !== null ? q : wb.bound;
    const merged0 = kind === 'KobCondAsk' && typeof d.merged_into === 'string';
    const proceeds = qt === null ? null : quote(n, net(qt), 'up');
    if (qt !== null && proceeds !== null && !(partial && merged0)) {
      out.payoutChecked = true;
      let required = proceeds;
      const merged = kind === 'KobCondAsk' && typeof d.merged_into === 'string';
      // the repeat merge returns ceil(n * rptPrice / scale) to the entry: the maker gets the rest
      if (merged) required -= quote(n, b0(s.rptPrice), 'up') ?? 0n;
      if (payout < required) {
        out.violations.push({
          invariant: INV.allIn,
          severity: 'error',
          subject,
          detail: {
            ...base,
            problem: merged ? 'maker profit below the all-in proceeds minus the re-armed budget' : 'maker payout below the all-in minimum',
            required: str(required),
            short: str(required - payout),
            quote: str(qt),
            quoteSource: verified ? 'event (covenant quote at t)' : 'worst bound',
            scale: str(scale),
            tip: str(tip),
            ...(merged ? { rptPrice: s.rptPrice, mergedInto: d.merged_into } : {}),
          },
        });
      }
    }
  }
  // bid side: the indexer reports the escrow before and after the fill (detail.escrow_before / escrow_after, absent when the bid sold
  // out) and the delivery output at the input's position (delivery_value: the maker's token output, whose KAS is the delivery carrier
  // plus, for a rising bid, the used(n) - allIn(t) that KobBid consumes at the budget rate but returns on the delivery). The
  // buy's cost is what left the maker: escrow_before - escrow_after - delivery_value <= floor(n * (q + tip) / scale).
  const eb = big(d.escrow_before);
  const ea = big(d.escrow_after) ?? 0n;
  const dv = big(d.delivery_value);
  if (n !== null && n > 0n && ev.side === 2 && kind === 'KobBid' && eb !== null && dv !== null && !partial) {
    const verified = d.verified === true && d.t !== undefined && d.t !== null;
    const qt = verified && q !== null ? q : wb.bound;
    const cap = qt === null ? null : quote(n, qt + tip, 'down');
    if (qt !== null && cap !== null) {
      out.payoutChecked = true;
      const drawn = eb - ea - dv;
      if (drawn > cap) {
        out.violations.push({
          invariant: INV.allIn,
          severity: 'error',
          subject,
          detail: { ...base, problem: 'buy cost more than its all-in limit', cost: str(drawn), cap: str(cap), escrowBefore: str(eb), escrowAfter: str(ea), deliveryValue: str(dv), quote: str(qt) },
        });
      }
    }
  }
  return out;
}

/**
 * Terms of an order whose proven state is gone (terminated before the checker saw it live), from the view's top-level fields:
 * `price`, `tip`, `scale`, `min_fill`, `token`, `active_from`; a bid's cap from `budget_rate = priceMax + tip` (a rising bid when priceMax is above
 * `price`). Marked `_partial`: `checkFill` then checks the bound of bids only and the payout against the event's verified quote.
 */
export function partialTerms(v: { price?: string | null; tip?: string | null; scale?: number | null; min_fill?: string | null; budget_rate?: string | null; tif?: number | null; token?: string | null; active_from?: number | null }): Record<string, string> | null {
  if (v.price == null || v.scale == null) return null;
  const t: Record<string, string> = { _partial: '1', price: v.price, tip: v.tip ?? '0', scale: String(v.scale), slope: '0', priceEnd: '0' };
  if (v.min_fill != null) t.minFill = v.min_fill;
  if (v.tif != null) t.tif = String(v.tif);
  if (v.token) t.tokenCovId = v.token;
  if (v.active_from != null) t.activeFrom = String(v.active_from);
  const br = big(v.budget_rate);
  if (br !== null) {
    const cap = br - b0(t.tip);
    if (cap > b0(v.price)) {
      t.slope = '1';
      t.priceEnd = cap.toString();
    }
  }
  return t;
}

/** a `KobIfdAsk` entry fill pays its proceeds into the exit it creates: `exit value >= ceil(n * (q - tip) / scale)` (lower bound; the prefund and carriers come on top) */
export function checkIfdAskExitFunding(entry: { covenant_id: string }, s: Record<string, string>, ev: EventLike, exit: OrderLike | null): Violation | null {
  if (ev.kind !== 'fill' || !exit || !exit.current || exit.current.txid !== ev.txid || exit.current.value == null) return null;
  const n = big(ev.amount);
  const q = big(ev.price) ?? b0(s.price);
  if (n === null || n <= 0n) return null;
  const rate = q > b0(s.tip) ? q - b0(s.tip) : 0n;
  const required = quoteOf(n, rate, b0(s.scale), 'up');
  if (required === null) return null;
  const have = b0(exit.current.value);
  if (have >= required) return null;
  return {
    invariant: INV.allIn,
    severity: 'error',
    subject: `${entry.covenant_id}:${ev.txid}`,
    detail: { order: entry.covenant_id, exit: exit.covenant_id, txid: ev.txid, amount: ev.amount, price: ev.price, problem: 'sell-first entry fill: exit holds less than the all-in proceeds', exitValue: str(have), required: str(required) },
  };
}

// ------------------------------------------------------------------------------------------------ 4. IOC / FOK

export const IOC_LIFE = 600n;

/** kill time: the indexer's `kill_daa`, else `min(expiryDaa, max(UTXO DAA, activeFrom) + 600)` */
export function killDaa(o: OrderLike): bigint | null {
  if (o.kill_daa !== null && o.kill_daa !== undefined) return BigInt(o.kill_daa);
  const utxo = big(o.current_daa ?? o.last_daa);
  if (utxo === null) return null;
  const af = b0(o.active_from ?? o.state?.state.activeFrom);
  const k = maxB(utxo, af) + IOC_LIFE;
  const exp = big(o.expiry_daa ?? o.state?.state.expiryDaa);
  return exp !== null && exp > 0n ? minB(exp, k) : k;
}

/**
 * invariant 4 on a live order: an IOC / FOK (tif 1 / 2) still open `grace` DAA after its kill time; a FOK in status `partial`.
 *
 * `cursorDaa` is the INDEXER's cursor (the DAA the keepers' book has reached), never the node's: a keeper cannot act on an order
 * its indexer has not seen. `followingSince` is the cursor DAA at which the indexer last started following the tip within the
 * matcher's tolerance (null while it lags or is down: nothing is overdue then, because the matcher and keepers plan nothing
 * while the book lags the node). The deadline is `max(kill, followingSince, born) + grace`, `born` being the DAA of the order's current
 * UTXO: the keepers get the grace from the moment they can act, so an indexer that has just caught up past an order's kill time is not
 * blamed for the lag, and an order whose creation was accepted after its own expiry (a transaction that waited minutes in a full
 * mempool at the relay-floor fee rate, TN10 after a flood) is not overdue before it exists.
 */
export function checkKill(o: OrderLike, cursorDaa: bigint, grace = 1200n, followingSince: bigint | null = 0n): Violation[] {
  const out: Violation[] = [];
  if ((o.tif !== 1 && o.tif !== 2) || !isActive(o.status)) return out;
  const k = killDaa(o);
  const born = big(o.current_daa ?? o.last_daa);
  const from = k === null || followingSince === null ? null : maxB(maxB(k, followingSince), born ?? 0n);
  if (k !== null && from !== null && cursorDaa > from + grace) {
    out.push({
      invariant: INV.iocFok,
      severity: 'error',
      subject: `${o.covenant_id}:kill`,
      detail: { order: o.covenant_id, contract: o.contract, tif: o.tif, status: o.status, problem: 'IOC/FOK not killed on time', killDaa: str(k), cursorDaa: str(cursorDaa), followingSince: str(followingSince), bornDaa: str(born), overdueDaa: str(cursorDaa - k), overdueSinceFollowingDaa: str(cursorDaa - from), current: o.current },
    });
  }
  if (o.tif === 2 && o.status === 'partial') {
    out.push({ invariant: INV.iocFok, severity: 'error', subject: `${o.covenant_id}:fok-partial`, detail: { order: o.covenant_id, contract: o.contract, problem: 'FOK partially filled (status partial)', filled: o.filled_amount, remaining: o.amount_left } });
  }
  return out;
}

/** invariant 4 on the events of a FOK: every fill closes the order, and no remainder is returned in a fill's transaction */
export function checkFokEvents(o: { covenant_id: string; contract: string; tif?: number | null }, events: EventLike[]): Violation[] {
  if (o.tif !== 2) return [];
  const out: Violation[] = [];
  const fillTx = new Set(events.filter((e) => e.kind === 'fill').map((e) => e.txid));
  for (const e of events) {
    if (e.kind === 'fill' && !e.closes) {
      out.push({ invariant: INV.iocFok, severity: 'error', subject: `${o.covenant_id}:fok-partial:${e.txid}`, detail: { order: o.covenant_id, contract: o.contract, txid: e.txid, amount: e.amount, problem: 'FOK fill did not consume the whole order' } });
    }
    if (e.kind === 'kill' && fillTx.has(e.txid)) {
      out.push({ invariant: INV.iocFok, severity: 'error', subject: `${o.covenant_id}:fok-remainder:${e.txid}`, detail: { order: o.covenant_id, txid: e.txid, returned: e.amount, problem: 'FOK filled partially with the remainder returned' } });
    }
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ 5. triggers (touch, protocol v2.6)

/**
 * One fill of the transaction of an `arm` / `trail` event that could be its trigger evidence (the fill of ANOTHER order in the same
 * transaction). `contract` is the filled order's kind: only a plain `KobAsk` / `KobBid` (or Kron) fill is evidence. `exposedSince` is
 * `max(order UTXO DAA + interval, custody UTXO DAA (asks only), activeFrom)` or null when the data to compute it is missing;
 * `slope` null = unknown (the proven state of the order is gone).
 */
export interface EvidenceFact {
  order?: string;
  contract: string;
  /** 1 ask (a resting ask was filled), 2 bid */
  side: number;
  tokenCovId: string;
  /** base units per whole token of the evidence order (touch evidence compares prices of one scale only) */
  scale: string | number;
  price: string | number;
  /** base units filled in the transaction */
  amount: string | number;
  slope: string | number | null;
  exposedSince: string | number | null;
  /**
   * when `exposedSince` is unknown: a DAA the exposure cannot have started before (the start computed without a term the data lacks,
   * e.g. a TWAP `interval` of an order whose state is gone). Exposure from the floor already short of `minRestDaa` is a violation.
   */
  exposedFloor?: string | number | null;
}

export interface TriggerCheck {
  ok: boolean;
  violations: Violation[];
  /** why each candidate evidence fill failed */
  reasons: string[];
}

/**
 * Invariant 5 (touch trigger, v2.6): an `arm` / `trail` event of a conditional order or stop entry must be backed by a fill (n > 0) in
 * the SAME transaction of a plain resting `KobAsk` / `KobBid` (or Kron) of the same token and scale (`evidence`, contracts/v2 `touch`):
 *   common      amount (base units) >= minTouch; slope == 0; exposedSince + minRestDaa <= the tx's DAA (the event's `daa` is
 *               never below the lock time, so failing it is certain); the tx's DAA >= activeFrom
 *   arm         KobCondAsk / KobIfdAsk: a resting ASK (side 1) with price <= stop (entryStop);
 *               KobCondBid / KobIfdBid: a resting BID (side 2) with price >= stop (entryStop)
 *   trail       KobCondAsk: a resting BID, k = floor((rp - trailGap - oldStop) / trailStep) >= 1, so the new stop <= rp - trailGap (and,
 *               without a take-profit cap, > rp - trailGap - trailStep); KobCondBid: a resting ASK, mirrored downwards.
 *               The new stop is the current `stopPrice` when this trail is the latest stop change (`latestStopChange`), else only the
 *               side is checked.
 * A candidate that passes everything but whose exposure (or slope) cannot be verified from the indexer data yields a 'warn'
 * ("unverifiable"), not an error; no candidate at all, or none that could pass, is an error.
 */
export function checkTrigger(
  order: { covenant_id: string; contract: string },
  s: Record<string, string>,
  ev: EventLike,
  evidence: EvidenceFact[],
  o: { latestStopChange?: boolean; stopUnknown?: boolean } = {},
): TriggerCheck {
  const kind = baseKind(order.contract);
  const reasons: string[] = [];
  const subject = `${order.covenant_id}:${ev.kind}:${ev.txid}`;
  const stopField = kind === 'KobIfdBid' || kind === 'KobIfdAsk' ? 'entryStop' : 'stopPrice';
  const stop = b0(s[stopField]);
  const txDaa = BigInt(ev.daa);
  let unverifiable: string[] | null = null;
  for (const r of evidence) {
    const why: string[] = [];
    const unknown: string[] = [];
    const rp = b0(r.price);
    const side = Number(r.side);
    const rk = baseKind(r.contract);
    if (rk !== 'KobAsk' && rk !== 'KobBid') why.push(`${r.contract} fill is not evidence (only a plain resting KobAsk / KobBid)`);
    else if ((rk === 'KobAsk' ? 1 : 2) !== side) why.push(`side ${side} does not match ${r.contract}`);
    if ((r.tokenCovId ?? '').toLowerCase() !== (s.tokenCovId ?? '').toLowerCase()) why.push('token');
    if (b0(r.scale) !== b0(s.scale)) why.push('scale');
    if (b0(r.amount) <= 0n) why.push('no amount filled');
    if (b0(r.amount) < b0(s.minTouch)) why.push(`amount ${b0(r.amount)} < minTouch ${s.minTouch}`);
    if (r.slope === null || r.slope === undefined) unknown.push('slope of the evidence order unknown');
    else if (b0(r.slope) !== 0n) why.push('decaying / rising evidence order (slope != 0)');
    if (r.exposedSince === null || r.exposedSince === undefined) {
      if (r.exposedFloor != null && b0(r.exposedFloor) + b0(s.minRestDaa) > txDaa) why.push(`exposed since ${r.exposedFloor} at the earliest: ${txDaa - b0(r.exposedFloor)} DAA < minRestDaa ${s.minRestDaa} at the tx (DAA ${ev.daa})`);
      else unknown.push('exposure of the evidence order unknown');
    }
    else if (b0(r.exposedSince) + b0(s.minRestDaa) > txDaa) why.push(`exposed since ${r.exposedSince}: ${txDaa - b0(r.exposedSince)} DAA < minRestDaa ${s.minRestDaa} at the tx (DAA ${ev.daa})`);
    if (txDaa < b0(s.activeFrom)) why.push(`tx DAA ${ev.daa} < activeFrom ${s.activeFrom}`);
    if (ev.kind === 'arm' || ev.kind === 'fill') {
      // an arm, or a fill at the trigger (a triggered stop leg / stop entry, `detail.evidence` on the fill): the same side rule
      const beyond = (msg: string) => (o.stopUnknown ? unknown.push(`${msg} (the stop may have trailed since the terms were read)`) : why.push(msg));
      if (kind === 'KobCondAsk' || kind === 'KobIfdAsk') {
        if (side !== 1) why.push(`side ${side} (${ev.kind} needs a filled resting ask, side 1)`);
        else if (rp > stop) beyond(`price ${rp} above the stop ${stop}`);
      } else if (kind === 'KobCondBid' || kind === 'KobIfdBid') {
        if (side !== 2) why.push(`side ${side} (${ev.kind} needs a filled resting bid, side 2)`);
        else if (rp < stop) beyond(`price ${rp} below the stop ${stop}`);
      } else why.push(`${order.contract} cannot be ${ev.kind === 'arm' ? 'armed' : 'triggered'}`);
    } else if (ev.kind === 'trail') {
      const gap = b0(s.trailGap);
      const step = b0(s.trailStep);
      const uncapped = b0(s.tpPrice) === 0n;
      if (step <= 0n) why.push('trail on an order without trailStep');
      if (kind === 'KobCondAsk') {
        if (side !== 2) why.push(`side ${side} (a sell stop trails on a filled resting bid, side 2)`);
        else if (o.latestStopChange && rp - gap < stop) why.push(`price ${rp} - gap ${gap} below the new stop ${stop}`);
        else if (o.latestStopChange && step > 0n && uncapped && rp - gap - step >= stop) why.push(`price ${rp} - gap ${gap} justified a higher stop than ${stop} (step ${step})`);
      } else if (kind === 'KobCondBid') {
        if (side !== 1) why.push(`side ${side} (a buy stop trails on a filled resting ask, side 1)`);
        else if (o.latestStopChange && rp + gap > stop) why.push(`price ${rp} + gap ${gap} above the new stop ${stop}`);
        else if (o.latestStopChange && step > 0n && uncapped && rp + gap + step <= stop) why.push(`price ${rp} + gap ${gap} justified a lower stop than ${stop} (step ${step})`);
      } else why.push(`${order.contract} cannot trail`);
    }
    if (!why.length) {
      if (!unknown.length) return { ok: true, violations: [], reasons };
      unverifiable = [...(unverifiable ?? []), ...unknown];
      reasons.push(`unverifiable: ${unknown.join('; ')}`);
    } else reasons.push(why.join('; '));
  }
  if (unverifiable) {
    return {
      ok: false,
      violations: [{ invariant: INV.trigger, severity: 'warn', subject, detail: { order: order.covenant_id, contract: order.contract, event: ev.kind, txid: ev.txid, daa: ev.daa, problem: 'trigger evidence cannot be verified from the indexer data', unverifiable, evidence } }],
      reasons,
    };
  }
  return {
    ok: false,
    violations: [
      {
        invariant: INV.trigger,
        severity: 'error',
        subject,
        detail: {
          order: order.covenant_id,
          contract: order.contract,
          event: ev.kind,
          txid: ev.txid,
          daa: ev.daa,
          stop: str(stop),
          problem: evidence.length ? 'no fill in the transaction satisfies the touch trigger rule' : 'no resting KobAsk / KobBid fill in the transaction of the arm / trail',
          reasons,
          evidence,
        },
      },
    ],
    reasons,
  };
}

// ------------------------------------------------------------------------------------------------ 6. repeat IFD / IFO

/** a booked exit of `entryId` */
export const isBookedExitOf = (o: OrderLike, entryId: string): boolean => (o.state?.state.parent ?? '').toLowerCase() === entryId.toLowerCase();

/** entry amount left + live booked exit amounts left (base units) never exceed N (the entry's initial amount) */
export function checkRepeatAmount(entry: OrderLike, n: string | number | bigint | null, exits: OrderLike[]): Violation | null {
  const total = big(n);
  if (total === null || !entry.state) return null;
  const el = b0(entry.state.state.amountLeft);
  let xl = 0n;
  const live: Record<string, string> = {};
  for (const x of exits) {
    if (!isActive(x.status) || !isBookedExitOf(x, entry.covenant_id) || !x.state) continue;
    xl += b0(x.state.state.amountLeft);
    live[x.covenant_id] = x.state.state.amountLeft;
  }
  if (el + xl <= total) return null;
  return {
    invariant: INV.repeat,
    severity: 'error',
    subject: `${entry.covenant_id}:amount`,
    detail: { entry: entry.covenant_id, problem: 'entry amount + booked exit amounts exceed N', n: str(total), entryAmount: str(el), exitAmount: str(xl), exits: live, entryOutpoint: entry.current },
  };
}

/** every `rearm` of an entry pairs with a take-profit fill of the named exit in the same transaction, of the same amount, merged into it */
export function checkRearm(entryId: string, rearm: EventLike, exitEvents: EventLike[] | null): Violation | null {
  const d = detailOf(rearm);
  const exit = typeof d.exit === 'string' ? d.exit : null;
  const base = { entry: entryId, exit, txid: rearm.txid, amount: rearm.amount };
  if (!exit) return { invariant: INV.repeat, severity: 'error', subject: `${entryId}:rearm:${rearm.txid}`, detail: { ...base, problem: 'rearm names no exit' } };
  if (!exitEvents) return null;
  const fill = exitEvents.find((e) => e.kind === 'fill' && e.txid === rearm.txid);
  if (!fill) return { invariant: INV.repeat, severity: 'error', subject: `${entryId}:rearm:${rearm.txid}`, detail: { ...base, problem: 'rearm without a take-profit fill of its exit in the same transaction' } };
  const fd = detailOf(fill);
  if (fill.amount !== rearm.amount || fd.merged_into !== entryId) {
    return { invariant: INV.repeat, severity: 'error', subject: `${entryId}:rearm:${rearm.txid}`, detail: { ...base, problem: 'rearm does not match the exit fill', fillAmount: fill.amount, mergedInto: fd.merged_into } };
  }
  return null;
}

/**
 * Repeat economics of one merged take-profit (buy-first: `KobIfdBid` entry, `KobCondAsk` exit): the exit's `rptPrice` is the entry's
 * budget rate `price + tip` (sompi per whole token), the fill is the take-profit leg (`q == tpPrice`), and the maker keeps at least
 * `ceil(m * (tp - tip_exit) / scale) - ceil(m * rptPrice / scale)` (the all-in proceeds minus what the merge returns to the entry): the
 * configured spread. Sell-first (`KobIfdAsk` + `KobCondBid`): `rptPrice == price - tip`, `rptPre == prefund`; the profit is paid by a bid-side
 * fill, which the read API does not value (not checked).
 */
export function checkRepeatCycle(entry: OrderLike, exit: OrderLike, fill: EventLike): Violation[] {
  const out: Violation[] = [];
  const e = entry.state?.state;
  const x = exit.state?.state;
  if (!e || !x || fill.kind !== 'fill') return out;
  const subject = `${exit.covenant_id}:cycle:${fill.txid}`;
  const base = { entry: entry.covenant_id, exit: exit.covenant_id, txid: fill.txid, amount: fill.amount };
  const ek = baseKind(entry.contract);
  if (ek === 'KobIfdBid') {
    const budget = b0(e.price) + b0(e.tip);
    if (b0(x.rptPrice) !== budget) out.push({ invariant: INV.repeat, severity: 'error', subject: `${exit.covenant_id}:rptPrice`, detail: { ...base, problem: 'booked exit rptPrice != entry budget rate (price + tip)', rptPrice: x.rptPrice, budget: str(budget) } });
    const q = big(fill.price);
    const tp = b0(x.tpPrice);
    if (q !== null && q !== tp) out.push({ invariant: INV.repeat, severity: 'error', subject: `${subject}:leg`, detail: { ...base, problem: 'merged fill not at the take-profit price', price: fill.price, tpPrice: x.tpPrice } });
    const m = big(fill.amount);
    const payout = big(fill.payout);
    const scale = b0(x.scale);
    const tipX = b0(x.tip);
    const proceeds = m === null ? null : quoteOf(m, tp > tipX ? tp - tipX : 0n, scale, 'up');
    const returned = m === null ? null : quoteOf(m, b0(x.rptPrice), scale, 'up');
    if (m !== null && payout !== null && proceeds !== null && returned !== null) {
      const profit = proceeds - returned;
      if (payout < profit) {
        out.push({ invariant: INV.repeat, severity: 'error', subject, detail: { ...base, problem: 'maker profit of the cycle below the configured spread', payout: fill.payout, minProfit: str(profit), tpPrice: x.tpPrice, entryPrice: e.price } });
      }
    }
  } else if (ek === 'KobIfdAsk') {
    const proceedsRate = b0(e.price) - b0(e.tip);
    if (b0(x.rptPrice) !== proceedsRate || b0(x.rptPre) !== b0(e.prefund)) {
      out.push({ invariant: INV.repeat, severity: 'error', subject: `${exit.covenant_id}:rptPrice`, detail: { ...base, problem: 'booked exit rptPrice/rptPre != entry proceeds rate (price - tip) / prefund', rptPrice: x.rptPrice, rptPre: x.rptPre, proceedsRate: str(proceedsRate), prefund: e.prefund } });
    }
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ 8. fee == recorded rate x mass, rate within [floor, maxRate]

export interface TxLine {
  ts?: number;
  bot?: string;
  what?: string;
  txid: string;
  fee: string;
  minFee?: string;
  feeRate: string;
  feeMode: string;
  compute: number | string;
  transientNormalized: number | string;
  storage?: number | string;
  /** index of the change output; null when the builder folded a change too small to carry (its storage mass) into the fee */
  changeOutput?: number | null;
  /** the fee policy's view of the transaction (informational: the checker never trusts it for the bounds) */
  urgency?: string | null;
  feeSource?: string | null;
}

/** The rates a fee may legitimately use: the relay floor and the policy's maximum rate (sompi per gram). */
export interface FeeBounds {
  floor: bigint;
  maxRate: bigint;
}

/** kob_protocol MIN_FEE_RATE and the policy default (fee-policy.ts DEFAULT_FEE_POLICY.maxRate). */
export const DEFAULT_FEE_BOUNDS: FeeBounds = { floor: 100n, maxRate: 1000n };

/** Block storage-mass limit and mass parameter (kob-protocol `tx.rs:49-51`: `STORAGE_MASS_PARAMETER`, `BLOCK_STORAGE_LIMIT`). */
export const STORAGE_MASS_PARAMETER = 1_000_000_000_000n;
export const BLOCK_STORAGE_LIMIT = 500_000n;
/**
 * Mass a P2PK change output (34-byte script, no covenant) adds to a transaction, from rusty-kaspa v2.1.0 `consensus/core/src/mass/mod.rs`:
 * serialized size 8 (value) + 2 (spk version) + 8 (spk length) + 34 = 52 bytes (`transaction_output_estimated_serialized_size`);
 * compute = 52 (per tx byte) + 10 x (2 + 34) (per spk byte) = 412; transient = 4 x size, normalized with the mempool cofactor 1/2 = 2 x size = 104.
 * Its storage plurality is 1 (`utxo_plurality`: 63 + 34 <= 100).
 */
export const CHANGE_OUTPUT_COMPUTE_MASS = 412n;
export const CHANGE_OUTPUT_TRANSIENT_NORMALIZED = 104n;

/**
 * The exact rule the builder folds a change by (kob-protocol `tx.rs` `Draft::seal`, lines 1229-1266): it solves the change value
 * `c = slack - fee(with change output)` and keeps the change only if the transaction WITH it is `within_block_limits()`
 * (compute <= 500000, storage <= 500000, transient <= 1000000, `tx.rs:541`); otherwise the change is folded into the fee
 * (`change_output = None`, the whole slack is paid). Hence, in relay mode (fee = 100 x max(compute, transientNormalized)):
 *
 *   folded  <=>  c <= 0  or  storage(tx with change c) > 500000
 *   excess  =  fee - floor  =  slack - fee_without  =  c + dFee     (dFee = floor(with change output) - floor(without))
 *
 * The storage mass with the change is `D + floor(C / c)` (`calc_storage_mass`: harmonic outputs `C p^2 / amount` per output; the change has
 * p = 1), where `D` is the storage mass of the transaction without the change (max(0, others - inputs)), which txs.jsonl records as
 * `storage`. So a change is folded iff `floor(C / c) > 500000 - D`, i.e. `c <= floor(C / (500001 - D))`; when `D` is clamped to 0 the least
 * favourable case is D = 0 (c <= 1_999_996). The largest excess a fold can leave is therefore `floor(C / (500001 - storage)) + dFee`.
 * (Assumption: adding the change does not switch the input side from the harmonic to the arithmetic form of `calc_storage_mass`, which
 * needs more than 2 inputs and one other output; the bots' amends have 1-2 inputs. The compute and transient limits are not reachable
 * by one output on transactions the wallet signs, so they are not part of the bound.)
 * Returns null when the transaction without change already exceeds the storage limit (no bound; such a line is never explained).
 */
export function maxFoldedExcess(l: Pick<TxLine, 'compute' | 'transientNormalized' | 'storage'>, rate = 100n): bigint | null {
  const storage = b0(l.storage ?? 0);
  if (storage > BLOCK_STORAGE_LIMIT) return null;
  const c0 = b0(l.compute);
  const t0 = b0(l.transientNormalized);
  const dFee = rate * (maxB(c0 + CHANGE_OUTPUT_COMPUTE_MASS, t0 + CHANGE_OUTPUT_TRANSIENT_NORMALIZED) - maxB(c0, t0));
  return STORAGE_MASS_PARAMETER / (BLOCK_STORAGE_LIMIT + 1n - storage) + dFee;
}

/** A fee above the floor that the builder's change fold explains (logged as an info line, not an incident). */
export interface FoldedFee {
  kind: 'folded';
  txid: string;
  bot?: string;
  what?: string;
  fee: string;
  expected: string;
  /** fee - floor: the change (plus the cost of the change output) folded into the fee */
  excess: string;
  /** the largest excess the fold rule allows for this transaction */
  maxFold: string;
}

/**
 * `fee == feeRate * max(compute, transientNormalized)` in relay mode (exact), with the RECORDED feeRate inside `bounds` ([relay floor, the policy's
 * maximum rate]); other modes: 'unchecked'. A dynamic fee (rate above the floor) is therefore not an incident; a rate outside the bounds, a fee that is not
 * its recorded rate times the independently measured mass, or a builder `minFee` that disagrees with it still is.
 * With no change output (`changeOutput === null`) an excess `0 < excess <= maxFoldedExcess` is the builder's fold (returned as a FoldedFee);
 * any other excess, a fee below its rate's cost, or an excess with a change output present (or an old line without the field) is a violation.
 */
export function checkFeeLine(l: TxLine, bounds: FeeBounds = DEFAULT_FEE_BOUNDS): Violation | null | 'unchecked' | FoldedFee {
  if (l.feeMode !== 'relay') return 'unchecked';
  const mass = maxB(b0(l.compute), b0(l.transientNormalized));
  const rate = b0(l.feeRate);
  const inBounds = rate >= bounds.floor && rate <= (bounds.maxRate < bounds.floor ? bounds.floor : bounds.maxRate);
  const expected = rate * mass;
  const fee = b0(l.fee);
  if (l.changeOutput === null && fee > expected && inBounds && (l.minFee === undefined || b0(l.minFee) === expected)) {
    const bound = maxFoldedExcess(l, rate);
    if (bound !== null && fee - expected <= bound) {
      return { kind: 'folded', txid: l.txid, bot: l.bot, what: l.what, fee: fee.toString(), expected: expected.toString(), excess: (fee - expected).toString(), maxFold: bound.toString() };
    }
  }
  const problems: string[] = [];
  if (!inBounds) problems.push(`feeRate ${l.feeRate} outside [${bounds.floor}, ${bounds.maxRate < bounds.floor ? bounds.floor : bounds.maxRate}] sompi per gram`);
  if (fee !== expected) problems.push(`fee ${fee} != ${l.feeRate} x max(compute ${l.compute}, transientNormalized ${l.transientNormalized}) = ${expected}`);
  if (l.minFee !== undefined && b0(l.minFee) !== expected) problems.push(`builder minFee ${l.minFee} != ${l.feeRate} x mass = ${expected}`);
  if (!problems.length) return null;
  return {
    invariant: INV.fee,
    severity: 'error',
    subject: l.txid,
    detail: { txid: l.txid, bot: l.bot, what: l.what, fee: l.fee, minFee: l.minFee, feeRate: l.feeRate, bounds: `${bounds.floor}..${bounds.maxRate}`, urgency: l.urgency, feeSource: l.feeSource, compute: l.compute, transientNormalized: l.transientNormalized, storage: l.storage, expected: str(expected), excess: str(fee - expected), problems },
  };
}

// ------------------------------------------------------------------------------------------------ 7. x402 settled exactly once

export interface Payment {
  ts: number;
  path: string;
  txid: string;
  amount?: string;
  asset?: string;
}
export interface LedgerEntry {
  txid: string;
  state: string;
  kind?: string;
  amount?: string;
  requestHash?: string;
  paymentId?: string;
  consumed?: { txid: string; index: number }[];
  createdMs?: number;
  updatedMs?: number;
  reason?: string;
}

/** replays a facilitator ledger (JSONL `{"entry": {...}}`, the last line of a txid wins); a torn line is skipped */
export function replayLedger(text: string): { last: Map<string, LedgerEntry>; requestHashes: Map<string, Set<string>>; lines: number; bad: number } {
  const last = new Map<string, LedgerEntry>();
  const requestHashes = new Map<string, Set<string>>();
  let lines = 0;
  let bad = 0;
  for (const raw of text.split('\n')) {
    const t = raw.trim();
    if (!t) continue;
    lines++;
    try {
      const e = (JSON.parse(t) as { entry?: LedgerEntry }).entry;
      if (!e?.txid) {
        bad++;
        continue;
      }
      last.set(e.txid, e);
      if (e.requestHash) {
        const s = requestHashes.get(e.txid) ?? new Set<string>();
        s.add(e.requestHash);
        requestHashes.set(e.txid, s);
      }
    } catch {
      bad++;
    }
  }
  return { last, requestHashes, lines, bad };
}

export interface X402Result {
  violations: Violation[];
  stats: { payments: number; settled: number; pending: number; ledgerEntries: number; ledgerAccepted: number; ledgerFailed: number; ledgerAmbiguous: number; unrecordedAccepted: number; byKind: Record<string, number> };
}

/**
 * Invariant 7: every payment the payer got a 200 for is in the facilitator ledger exactly once and `accepted` (grace `graceMs` for
 * pending / broadcast), no payment txid was recorded twice, no ledger txid carries two request hashes, and no outpoint was consumed by
 * two accepted settlements.
 */
export function checkX402(payments: Payment[], ledgerText: string, nowMs: number, graceMs = 15 * 60_000): X402Result {
  const { last, requestHashes } = replayLedger(ledgerText);
  const v: Violation[] = [];
  const seen = new Map<string, number>();
  let settled = 0;
  let pending = 0;
  for (const p of payments) {
    seen.set(p.txid, (seen.get(p.txid) ?? 0) + 1);
    if (seen.get(p.txid) === 2) v.push({ invariant: INV.x402, severity: 'error', subject: `${p.txid}:dup`, detail: { txid: p.txid, path: p.path, problem: 'one payment transaction served twice (recorded twice by the payer)' } });
    const e = last.get(p.txid);
    const old = nowMs - p.ts > graceMs;
    if (!e) {
      if (old) v.push({ invariant: INV.x402, severity: 'error', subject: p.txid, detail: { txid: p.txid, path: p.path, amount: p.amount, problem: 'paid (HTTP 200) but not in the facilitator ledger' } });
      else pending++;
      continue;
    }
    if (e.state === 'accepted') settled++;
    else if (e.state === 'failed' || e.state === 'ambiguous') {
      v.push({ invariant: INV.x402, severity: e.state === 'failed' ? 'error' : 'warn', subject: `${p.txid}:${e.state}`, detail: { txid: p.txid, path: p.path, state: e.state, reason: e.reason, problem: `paid (HTTP 200) but the ledger entry is ${e.state}` } });
    } else if (old) v.push({ invariant: INV.x402, severity: 'warn', subject: `${p.txid}:${e.state}`, detail: { txid: p.txid, path: p.path, state: e.state, problem: 'settlement not final after the grace period' } });
    else pending++;
  }
  for (const [txid, hs] of requestHashes) {
    if (hs.size > 1) v.push({ invariant: INV.x402, severity: 'error', subject: `${txid}:requests`, detail: { txid, requestHashes: [...hs], problem: 'one transaction bound to two payment requests' } });
  }
  const consumedBy = new Map<string, string>();
  let acc = 0;
  let failed = 0;
  let amb = 0;
  const byKind: Record<string, number> = {};
  const recorded = new Set(payments.map((p) => p.txid));
  let unrecorded = 0;
  for (const e of last.values()) {
    if (e.state === 'failed') failed++;
    if (e.state === 'ambiguous') amb++;
    if (e.state !== 'accepted') continue;
    acc++;
    byKind[e.kind ?? '?'] = (byKind[e.kind ?? '?'] ?? 0) + 1;
    if (!recorded.has(e.txid) && nowMs - (e.updatedMs ?? 0) > graceMs) unrecorded++;
    for (const c of e.consumed ?? []) {
      const k = `${c.txid}:${c.index}`;
      const prev = consumedBy.get(k);
      if (prev && prev !== e.txid) v.push({ invariant: INV.x402, severity: 'error', subject: `${k}:double`, detail: { outpoint: k, txids: [prev, e.txid], problem: 'one outpoint consumed by two accepted settlements' } });
      consumedBy.set(k, e.txid);
    }
  }
  return { violations: v, stats: { payments: payments.length, settled, pending, ledgerEntries: last.size, ledgerAccepted: acc, ledgerFailed: failed, ledgerAmbiguous: amb, unrecordedAccepted: unrecorded, byKind } };
}

// ------------------------------------------------------------------------------------------------ 11. pair orders

/** A pair order (protocol v3: KobPair, KobCondPair, KobIfdPair; token A for token B at B base units per whole A). */
export const isPairContract = (contract: string): boolean => /^Kob(Pair|CondPair|IfdPair)$/.test(contract);

export interface PairCheck {
  violations: Violation[];
  /** the fill carried the pair fields this rule reads */
  checked: boolean;
  /** what the maker received above its guarantee (an ask) or paid below it (a bid), base units of B */
  surplus: bigint;
}

/**
 * Invariant 11 (pair orders): the founder price rule and the pair guarantee, for one fill event of a pair order (`GET /v1/fills`):
 *   * a pair fill NEVER sets a price: the event's `price` is null and `detail.pair.price_source` is `none` (its counterparty may be the route
 *     through the KAS books, netting or inventory: the route's KAS-book fills record their own KAS trades);
 *   * at the order's quote q at the fill (`detail.pair.price`, B base units per whole A) an ASK receives at least `ceil(n x q / scale(A))` of B
 *     (`amount_b` = the call's tOut; a sell-first entry: the proceeds put into its exit), a BID pays at most `floor(n x q / scale(A))`
 *     (`amount_b` = its sOut; a buy-first entry: the B it releases). The KAS tip never comes out of B.
 * A fill without `detail.pair` is a warning (the indexer could not read the call).
 */
export function checkPairFill(order: { covenant_id: string; contract: string }, ev: EventLike): PairCheck {
  const out: PairCheck = { violations: [], checked: false, surplus: 0n };
  if (ev.kind !== 'fill' || !isPairContract(order.contract)) return out;
  const subject = `${order.covenant_id}:pair:${ev.txid}`;
  const base = { order: order.covenant_id, contract: order.contract, txid: ev.txid, amount: ev.amount };
  if (ev.price !== null && ev.price !== undefined) {
    out.violations.push({ invariant: INV.pair, severity: 'error', subject: `${subject}:price`, detail: { ...base, problem: 'a pair fill carries a price (pair fills never set a price, candle or last price)', price: ev.price } });
  }
  const d = (detailOf(ev).pair ?? null) as Record<string, unknown> | null;
  if (!d) {
    out.violations.push({ invariant: INV.pair, severity: 'warn', subject, detail: { ...base, problem: 'pair fill without detail.pair' } });
    return out;
  }
  if (d.price_source !== 'none') {
    out.violations.push({ invariant: INV.pair, severity: 'error', subject: `${subject}:source`, detail: { ...base, problem: 'a pair fill names a price source', price_source: d.price_source } });
  }
  const n = big(d.amount_a ?? ev.amount);
  const q = big(d.price);
  const sA = big(d.a_scale);
  const b = big(d.amount_b);
  if (n === null || q === null || sA === null || b === null || sA <= 0n) return out;
  out.checked = true;
  if (n <= 0n) {
    out.violations.push({ invariant: INV.pair, severity: 'error', subject, detail: { ...base, problem: 'pair fill of zero or negative amount' } });
    return out;
  }
  if (d.side === 'ask') {
    const due = quoteOf(n, q, sA, 'up')!;
    if (b < due) out.violations.push({ invariant: INV.pair, severity: 'error', subject, detail: { ...base, problem: 'a pair ask received less than its guaranteed B', due: str(due), received: str(b), price: str(q) } });
    else out.surplus = b - due;
  } else {
    const most = quoteOf(n, q, sA, 'down')!;
    if (b > most) out.violations.push({ invariant: INV.pair, severity: 'error', subject, detail: { ...base, problem: 'a pair bid paid more B than its limit allows', most: str(most), paid: str(b), price: str(q) } });
    else out.surplus = most - b;
  }
  return out;
}

// ------------------------------------------------------------------------------------------------ 12. supply conservation per token

/**
 * Invariant 12: the live token UTXOs of a fixed-supply token always add up to its supply (the KCC-20 program neither mints nor burns; a
 * fill, pair fill, cancel or transfer only moves units). `live` = the indexer's live token UTXOs plus the holdings the indexer never
 * saw (genesis outputs, pre-run holdings the bots' trackers know) that are still in the node's UTXO set. `baseline` is the accepted
 * constant offset (a token issued before the indexers started: holdings no tracker knows); 0 for a token issued under the indexers.
 */
export function checkSupply(token: string, ticker: string, supply: bigint, live: bigint, baseline: bigint, parts: Record<string, unknown> = {}): Violation | null {
  const delta = live - supply;
  if (delta === baseline) return null;
  return {
    invariant: INV.supply,
    severity: 'error',
    subject: `${token}:${delta}`,
    detail: { token, ticker, problem: delta > baseline ? 'more live token units than the supply (minted)' : 'fewer live token units than the supply (burnt or lost)', supply: str(supply), live: str(live), delta: str(delta), baseline: str(baseline), ...parts },
  };
}

// ------------------------------------------------------------------------------------------------ 1b. amount accounting

/**
 * Amount accounting of a live order, in base units: `filled_amount + amount_left == initial_amount` for the kinds whose `amountLeft` only
 * ever decreases by fills (a plain ask and a KobPair; a repeat entry gets merges back, a bid has no amount). The indexer derives the
 * three numbers from different columns (the creation event, the fill events, the current state), so a missed or doubled fill, or a state
 * of another order, shows here exactly. null when the view does not carry them (a bid, a conditional order, a closed order).
 */
export function checkAmountBalance(o: OrderLike): Violation | null {
  const k = baseKind(o.contract);
  if ((k !== 'KobAsk' && k !== 'KobPair') || !isActive(o.status)) return null;
  const init = big(o.initial_amount);
  const filled = big(o.filled_amount);
  const left = big(o.amount_left);
  if (init === null || filled === null || left === null) return null;
  if (filled + left === init) return null;
  return {
    invariant: INV.custody,
    severity: 'error',
    subject: `${o.covenant_id}:amount`,
    detail: { order: o.covenant_id, contract: o.contract, status: o.status, problem: 'filled + amount left != initial amount', initial: str(init), filled: str(filled), left: str(left), delta: str(filled + left - init) },
  };
}

// ------------------------------------------------------------------------------------------------ 9. two indexers agree

/** fields of one order both indexers must agree on (once both are past its last event) */
export function diffOrder(a: OrderLike, b: OrderLike | null): string[] {
  if (!b) return ['missing in B'];
  const out: string[] = [];
  const cmp = (k: string, x: unknown, y: unknown) => {
    if (JSON.stringify(x ?? null) !== JSON.stringify(y ?? null)) out.push(`${k}: ${JSON.stringify(x ?? null)} != ${JSON.stringify(y ?? null)}`);
  };
  cmp('status', a.status, b.status);
  cmp('filled_amount', a.filled_amount, b.filled_amount);
  cmp('amount_left', a.amount_left, b.amount_left);
  cmp('current', a.current && `${a.current.txid}:${a.current.index}`, b.current && `${b.current.txid}:${b.current.index}`);
  cmp('state_known', a.state_known, b.state_known);
  cmp('last_daa', a.last_daa, b.last_daa);
  return out;
}

export function diffSets(a: Iterable<string>, b: Iterable<string>): { onlyA: string[]; onlyB: string[] } {
  const sa = new Set(a);
  const sb = new Set(b);
  return { onlyA: [...sa].filter((x) => !sb.has(x)), onlyB: [...sb].filter((x) => !sa.has(x)) };
}

// ------------------------------------------------------------------------------------------------ 10. health

/** problem codes of one indexer's `/v1/health` (null = unreachable) */
export function healthIssues(h: HealthLike | null, nowMs: number, o: { maxLagDaa?: number; maxProgressAgeMs?: number } = {}): string[] {
  if (!h) return ['unreachable'];
  const out: string[] = [];
  if (h.state !== 'following') out.push(`state ${h.state}`);
  if ((h.lag_daa ?? 0) > (o.maxLagDaa ?? 600)) out.push(`lag ${h.lag_daa} DAA`);
  if (h.last_progress_unix_ms !== undefined && nowMs - h.last_progress_unix_ms > (o.maxProgressAgeMs ?? 60_000)) out.push(`no progress for ${Math.round((nowMs - h.last_progress_unix_ms) / 1000)} s`);
  return out;
}

/** Prometheus textfile -> `name` (or `name{labels}`) -> value */
export function parseProm(text: string): Record<string, number> {
  const out: Record<string, number> = {};
  for (const raw of text.split('\n')) {
    const t = raw.trim();
    if (!t || t.startsWith('#')) continue;
    const m = /^([a-zA-Z_:][\w:]*(?:\{[^}]*\})?)\s+(\S+)/.exec(t);
    if (!m) continue;
    const v = Number(m[2]);
    if (Number.isFinite(v)) out[m[1]] = v;
  }
  return out;
}

/** problem codes of one executor's metrics file */
export function metricIssues(m: Record<string, number> | null, nowSec: number, o: { maxStepAgeSec?: number } = {}): string[] {
  if (!m) return ['no metrics file'];
  const out: string[] = [];
  if (m.kob_matcher_node_synced === 0) out.push('node not synced');
  if (m.kob_matcher_book_stale === 1) out.push(`book stale (lag ${m.kob_matcher_book_lag_daa ?? '?'} DAA)`);
  if (m.kob_matcher_last_step_seconds !== undefined && nowSec - m.kob_matcher_last_step_seconds > (o.maxStepAgeSec ?? 60)) out.push(`last step ${Math.round(nowSec - m.kob_matcher_last_step_seconds)} s ago`);
  if (m.kob_matcher_paused === 1) out.push('paused');
  return out;
}

// ------------------------------------------------------------------------------------------------ persistence (no benign races)

/**
 * Condition tracker: a condition becomes an incident only after it held for `rounds` consecutive observations (and at least `minMs`),
 * then once per episode (the subject carries the episode start, so a later episode is a new incident). Serializable (checker state).
 */
export class Persist {
  data: Record<string, { since: number; count: number; fired: boolean; sig?: string }>;
  constructor(data: Record<string, { since: number; count: number; fired: boolean; sig?: string }> = {}) {
    this.data = data;
  }
  /** `sig`: the condition's identity (e.g. the missing outpoint); a changed sig restarts the count */
  observe(key: string, bad: boolean, now: number, rounds = 2, minMs = 0, sig?: string): { fire: boolean; since: number } {
    if (!bad) {
      delete this.data[key];
      return { fire: false, since: now };
    }
    let d = this.data[key];
    if (!d || d.sig !== sig) d = this.data[key] = { since: now, count: 0, fired: false, sig };
    d.count++;
    if (!d.fired && d.count >= rounds && now - d.since >= minMs) {
      d.fired = true;
      return { fire: true, since: d.since };
    }
    return { fire: false, since: d.since };
  }
  /** forget keys not observed in this round (orders that went away) */
  retain(keep: (key: string) => boolean): void {
    for (const k of Object.keys(this.data)) if (!keep(k)) delete this.data[k];
  }
}

/**
 * Invariant 11 for an `arm` / `trail` event (or a fill that armed in itself) of a pair stop (KobCondPair, a KobIfdPair stop entry): its
 * `detail.evidence` names evidence of one of the two modes the covenant accepts: mode 0, two KAS-book fills (inputs [A, B], their quotes `a` and
 * `b` > 0, both orders plain KAS-book orders: KobAsk / KobBid of either family); mode 1, a resting KobPair of the pair filled in the same
 * transaction (inputs [k], its `price` > 0). `kinds` = the contracts of the evidence orders when the checker could read them (null: not read).
 * The covenant itself checks sides, quotes vs the stop, sizes and rest; this rule checks what the indexer recorded is one of the two modes.
 */
export function checkPairEvidence(order: { covenant_id: string; contract: string }, ev: EventLike, kinds: (string | null)[] | null = null): Violation | null {
  if (!isPairContract(order.contract)) return null;
  const ev0 = detailOf(ev).evidence as Record<string, unknown> | undefined;
  if (ev.kind !== 'arm' && ev.kind !== 'trail' && !(ev.kind === 'fill' && ev0)) return null;
  const subject = `${order.covenant_id}:evidence:${ev.txid}`;
  const bad = (problem: string, extra: Record<string, unknown> = {}): Violation => ({
    invariant: INV.pair, severity: 'error', subject, detail: { order: order.covenant_id, contract: order.contract, kind: ev.kind, txid: ev.txid, problem, evidence: ev0 ?? null, ...extra },
  });
  if (!ev0) return bad('a pair arm / trail without evidence');
  const inputs = Array.isArray(ev0.inputs) ? ev0.inputs : [];
  const pos = (v: unknown) => (big(v) ?? 0n) > 0n;
  if (ev0.mode === 0) {
    if (inputs.length !== 2 || !pos(ev0.a) || !pos(ev0.b)) return bad('mode 0 evidence needs two KAS-book fills with their quotes');
    if (kinds && kinds.some((k) => k !== null && !/^Kob(Ask|Bid)(Kron)?$/.test(k))) return bad('mode 0 evidence is not two plain KAS-book orders', { kinds });
    return null;
  }
  if (ev0.mode === 1) {
    if (inputs.length !== 1 || !pos(ev0.price)) return bad('mode 1 evidence needs one resting pair order and its price');
    if (kinds && kinds[0] !== null && kinds[0] !== 'KobPair') return bad('mode 1 evidence is not a resting KobPair', { kinds });
    return null;
  }
  return bad('evidence of an unknown mode');
}
