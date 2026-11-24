// The safeguards: the spending ledger, the caps, the withdrawal allow-list, the kill switch and the
// price band.

import { appendFileSync, closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, statSync } from 'node:fs';
import { dirname } from 'node:path';
import { big, kas, utcDay } from './units.mjs';

/**
 * A JSON-lines ledger of what each write takes out of the wallet.
 *
 * A send first writes `reserve`. The outcome then writes `settle` (the spend the CLI reported;
 * `outcome: "unknown"` when the reply was lost) or `release` (the CLI said nothing was sent). A
 * reserve with neither (a crash) and an unknown outcome keep counting until the UTC day rolls.
 * A torn line (a crash while writing) is skipped.
 */
export class Ledger {
  constructor(file) {
    this.file = file;
  }

  entries() {
    if (!existsSync(this.file)) return [];
    const out = [];
    for (const l of readFileSync(this.file, 'utf8').split(/\r?\n/)) {
      if (!l.trim()) continue;
      try {
        out.push(JSON.parse(l));
      } catch {
        // A torn line: a reserve torn by a crash was never followed by a send.
      }
    }
    return out;
  }

  append(e) {
    mkdirSync(dirname(this.file), { recursive: true });
    appendFileSync(this.file, `${this.endsTorn() ? '\n' : ''}${JSON.stringify({ ts: new Date().toISOString(), ...e })}\n`);
  }

  /** The file's last byte is not a newline (a torn line): the next entry starts a new line. */
  endsTorn() {
    if (!existsSync(this.file)) return false;
    const size = statSync(this.file).size;
    if (!size) return false;
    const fd = openSync(this.file, 'r');
    try {
      const b = Buffer.alloc(1);
      readSync(fd, b, 0, 1, size - 1);
      return b[0] !== 0x0a;
    } finally {
      closeSync(fd);
    }
  }

  reserve(id, day, spend, meta = {}) {
    this.append({ kind: 'reserve', id, day, spend: String(spend), ...meta });
  }

  settle(id, spend, txids, outcome) {
    this.append({ kind: 'settle', id, spend: String(spend), txids, ...(outcome ? { outcome } : {}) });
  }

  /**
   * Sends of the UTC day `day` whose outcome is unknown (a lost reply, a crash): they count as
   * spent.
   */
  unknownOn(day) {
    const byId = new Map();
    for (const e of this.entries()) {
      if (e.kind === 'reserve') byId.set(e.id, { id: e.id, day: e.day, tool: e.tool, spend: e.spend, txids: [], open: true });
      else if (byId.has(e.id)) {
        const v = byId.get(e.id);
        v.open = e.kind === 'settle' && e.outcome === 'unknown';
        if (e.txids) v.txids = e.txids;
        if (e.spend != null) v.spend = e.spend;
      }
    }
    return [...byId.values()].filter((v) => v.day === day && v.open).map(({ id, tool, spend, txids }) => ({ id, tool, spendKas: kas(BigInt(spend)), txids }));
  }

  release(id, reason) {
    this.append({ kind: 'release', id, reason });
  }

  /** Sompi spent on the UTC day `day` (reserves of that day, as settled, less the released). */
  spentOn(day) {
    const byId = new Map();
    for (const e of this.entries()) {
      if (e.kind === 'reserve') byId.set(e.id, { day: e.day, spend: BigInt(e.spend) });
      else if (e.kind === 'settle' && byId.has(e.id)) byId.get(e.id).spend = BigInt(e.spend);
      else if (e.kind === 'release' && byId.has(e.id)) byId.get(e.id).spend = 0n;
    }
    let sum = 0n;
    for (const v of byId.values()) if (v.day === day) sum += v.spend;
    return sum;
  }

  /**
   * Base units of token `cov` that left the wallet on the UTC day `day` (reserves not released).
   */
  tokensOutOn(day, cov) {
    const byId = new Map();
    for (const e of this.entries()) {
      if (e.kind === 'reserve') byId.set(e.id, e.day === day ? big(e.tokens?.[cov]) : 0n);
      else if (e.kind === 'release' && byId.has(e.id)) byId.set(e.id, 0n);
    }
    let sum = 0n;
    for (const v of byId.values()) sum += v;
    return sum;
  }

  /**
   * Token creations on the UTC day `day` that were not released (sent, or with an unknown outcome).
   */
  launchesOn(day) {
    const ids = new Set();
    for (const e of this.entries()) {
      if (e.kind === 'reserve' && e.day === day && e.tool === 'kob_token_create') ids.add(e.id);
      else if (e.kind === 'release') ids.delete(e.id);
    }
    return ids.size;
  }
}

/** The caps a write would break: an array of reasons, empty when it passes. */
export function capRefusals({ spend, fee }, caps, spentToday) {
  const out = [];
  if (spend > caps.perTx) out.push(`PER_TX_CAP: this takes ${kas(spend)} KAS out of the wallet, above the ${kas(caps.perTx)} KAS per transaction`);
  if (spentToday + spend > caps.perDay) {
    out.push(`DAILY_CAP: ${kas(spentToday)} KAS spent today (UTC) + ${kas(spend)} KAS is above the ${kas(caps.perDay)} KAS per day`);
  }
  if (fee > caps.maxFee) out.push(`MAX_FEE: the fee ${kas(fee)} KAS is above the ${kas(caps.maxFee)} KAS cap`);
  return out;
}

/**
 * The tokens a planned transaction takes out of the wallet: { covenant id: base units }, from its
 * net effect.
 */
export function tokenOutflows(doc) {
  const out = {};
  for (const [cov, n] of Object.entries(doc?.transaction?.net?.tokens ?? {})) {
    const v = big(n);
    if (v < 0n) out[cov.toLowerCase()] = String(-v);
  }
  return out;
}

/**
 * The token caps a write would break.
 * `outs` is tokenOutflows, `outToday(cov)` the base units out today and `holding(cov)` the wallet's
 * free holding confirmed on the node (null when not confirmed), read only for tokens without
 * their own cap.
 */
export async function tokenCapRefusals(outs, caps, outToday, holding) {
  const refusals = [];
  for (const [cov, s] of Object.entries(outs)) {
    const out = BigInt(s);
    const today = outToday(cov);
    const own = caps.byToken.get(cov);
    if (own) {
      if (out > own.perTx) refusals.push(`TOKEN_PER_TX_CAP: ${out} base units of ${cov} leave the wallet, above its ${own.perTx} per transaction`);
      if (today + out > own.perDay) refusals.push(`TOKEN_DAILY_CAP: ${today} base units of ${cov} left today + ${out} is above its ${own.perDay} per day`);
      continue;
    }
    if (caps.perTxPct == null) {
      refusals.push(`TOKEN_CAP: ${cov} has no cap in limits.tokens and limits.tokenDefault is off`);
      continue;
    }
    const free = await holding(cov);
    if (free == null) {
      refusals.push(`TOKEN_CAP: the wallet's holding of ${cov} is not confirmed on the node; set its cap in limits.tokens`);
      continue;
    }
    const day = free + today; // the holding at the start of the UTC day, less what came in
    if (out * 100n > caps.perTxPct * day) {
      refusals.push(`TOKEN_PER_TX_CAP: ${out} base units of ${cov} leave the wallet, above ${caps.perTxPct}% of the day's holding ${day}`);
    }
    if ((today + out) * 100n > caps.perDayPct * day) {
      refusals.push(`TOKEN_DAILY_CAP: ${today} base units of ${cov} left today + ${out} is above ${caps.perDayPct}% of the day's holding ${day}`);
    }
  }
  return refusals;
}

export const killSwitchOn = (cfg) => existsSync(cfg.killSwitchFile);

/** An address on the allow-list (compared lower-case and trimmed). */
export const allowListed = (cfg, address) => cfg.withdrawTo.includes(String(address ?? '').trim().toLowerCase());

/** A decimal number (25, "2.5") as an exact fraction [num, den]. */
function fraction(x) {
  const m = /^(\d+)(?:\.(\d+))?$/.exec(String(x).trim());
  if (!m) throw new Error(`not a decimal: ${x}`);
  const frac = m[2] ?? '';
  return [BigInt(m[1] + frac), 10n ** BigInt(frac.length)];
}

/** The default KAS a book-derived reference must stand behind, on the fills and on the resting side. */
export const DEFAULT_REF_DEPTH_KAS = 1000;

/**
 * kobd's own dust line (docs/api-reference.md, section 3.6): half the listing minimum, 0.5 KAS by
 * default. A kobd that applies it leaves dust out of its book and says so with `dust_orders`;
 * every row it returns then counts (R7T-3). Only against an indexer that does not say so does
 * kob-mcp leave out the rows worth less than this at their quote itself (R6T-4): no stricter line
 * of its own, so honest rows the indexer counts are never thrown away here.
 */
export const DUST_BELOW_SOMPI = 50_000_000n;

/**
 * The rows of a side kob-mcp reads for the reference (R6T-4): the 200 dust rows an older kobd may
 * mix into its best 200, plus 200. The API clamps `depth` to its `max_page_size` (200 by default)
 * without an error: at that cap 200 dust rows at the top still leave no row to count, and the band
 * refuses (fail-closed) and says so.
 */
export const REF_BOOK_ROWS = 400;

/** A resting order whose minimum fill is worth more than this (10 KAS) adds no depth: few can fill it. */
export const MAX_MIN_FILL_SOMPI = 1_000_000_000n;

const lt = (a, b) => a.num * b.den < b.num * a.den;

/**
 * The price a taker gets from a book row now (R4T-1), sompi per its scale, or null when a taker
 * cannot fill it now: its `quote`. An auction (an armed stop or if-done entry, a decaying ask, a
 * rising bid) rests at its quote, which may be far from its limit `price` (the start of a decay or
 * a rise). The filler of an auction picks a time no later than its transaction's, so an ask is
 * bought at the quote of the node's DAA score or lower and a bid sold into at that quote or higher:
 * the quote is never better for the wallet's trade than what a taker gets. An ask is not counted
 * at a far start any longer (R5T-2). A row that is not live, settled and of its side's kind counts
 * nothing, and so does one without a quote (an older indexer).
 */
function nowPrice(side, r) {
  if (!/^\d+$/.test(String(r.price ?? '')) || !/^\d+$/.test(String(r.quote ?? ''))) return null;
  if (r.settled !== true || r.expired === true || r.deadline_passed === true) return null;
  if (r.status !== 'open' && r.status !== 'partial') return null;
  const kind = sideOfKind(r.contract);
  if (kind && kind !== side) return null;
  const now = BigInt(r.quote);
  return now > 0n ? now : null;
}

/**
 * The book-derived reference (R2-T2), sompi per `scale` base units as { num, den, what }, or null
 * with `why`. It needs `depth` sompi on both of:
 * - the resting side an attacker must fill-risk to move it: for a buy the bids from the best down,
 *   for a sell the asks from the best up; the price where what a taker can fill now adds up to
 *   `depth`. Each row counts its `fillable_now` (R3T-1): a TWAP or DCA order only the slice that is
 *   due, not its whole `amount_left`, and a row without the field (an older indexer) nothing. A
 *   row is valued and priced at what a taker gets now (`nowPrice`, R4T-1), a bid at most its escrow.
 *   The wallet's own orders and orders whose minimum fill is worth more than 10 KAS do not count.
 * - the fill history, newest first: the VWAP of the fills that add up to `depth`.
 * A buy is judged against the lower of the two and a sell against the higher. Fills between two
 * keys of one owner cost only fees, so the fills alone never move the bound the attacker's way:
 * only `depth` of orders anyone can fill now does. The fills keep a book with no trading out.
 */
export function bookReference({ side, rows, fills, scale, depth, maker = null }) {
  if (scale == null) return { ref: null, why: 'the book has no price scale' };
  const sc = BigInt(scale);
  const own = String(maker ?? '').toLowerCase();
  let value = 0n;
  let rest = null;
  let unsized = 0;
  let dust = 0;
  // An indexer that applies the dust rule says so (`dust_orders`, omitted when it left none out
  // on a newer kobd: the guard below then changes nothing, its line being kobd's).
  const kobdDust = rows?.dust_orders != null;
  for (const r of (side === 'buy' ? rows?.bids : rows?.asks) ?? []) {
    if (own && String(r.maker ?? '').toLowerCase() === own) continue;
    if (r.fillable_now == null) unsized++;
    const now = nowPrice(side, r);
    if (now == null || r.scale == null || BigInt(r.scale) !== sc) continue;
    // What a taker can fill now, never more than is left; a bid at most the KAS of its escrow.
    const left = big(r.amount_left);
    const fillable = big(r.fillable_now) < left ? big(r.fillable_now) : left;
    // A minimum fill above the slice that is due cannot be met now; one worth over 10 KAS at the
    // higher of limit and quote leaves few who can fill it.
    const minFill = big(r.min_fill);
    if (minFill > fillable && fillable < left) continue;
    const high = big(r.price) > now ? big(r.price) : now;
    if ((minFill * high) / sc > MAX_MIN_FILL_SOMPI) continue;
    let worth = (fillable * now) / sc;
    if (side === 'buy' && r.cur_value != null && big(r.cur_value) < worth) worth = big(r.cur_value);
    if (!kobdDust && worth < DUST_BELOW_SOMPI) {
      dust++;
      continue;
    }
    value += worth;
    if (value >= depth) {
      rest = { num: now, den: 1n, what: `${kas(depth)} KAS of ${side === 'buy' ? 'bids' : 'asks'}` };
      break;
    }
  }
  if (!rest) {
    const crowd = dust ? `; ${dust} row(s) worth under 0.5 KAS (dust) count as nothing${(rows?.[side === 'buy' ? 'bids' : 'asks']?.length ?? 0) >= 200 ? ': they may crowd the real orders out of the read (a newer kobd leaves dust out of its books); set refPrice for this token' : ''}` : '';
    const old = unsized ? `; the indexer names no fillable_now for ${unsized} order(s) (an older kobd), and they count as nothing` : '';
    return { ref: null, why: `under ${kas(depth)} KAS of ${side === 'buy' ? 'bids' : 'asks'} fillable now rest on the book (the wallet's own and large-minimum orders left out${crowd}${old})` };
  }
  let vol = 0n;
  let units = 0n;
  let fill = null;
  for (const f of fills ?? []) {
    const price = big(f.price);
    const amount = big(f.amount);
    if (price <= 0n || amount <= 0n) continue;
    vol += amount * price;
    units += amount;
    if (vol / sc >= depth) {
      fill = { num: vol, den: units, what: `the VWAP of the last ${kas(depth)} KAS of fills` };
      break;
    }
  }
  if (!fill) return { ref: null, why: `under ${kas(depth)} KAS of fills in the recent history` };
  const lower = lt(fill, rest) ? fill : rest;
  const higher = lower === fill ? rest : fill;
  return { ref: side === 'buy' ? lower : higher };
}

/**
 * The reference that bounds the band: the operator's own price for the token when set (sompi per
 * whole token), which a book reference may only tighten (a buy takes the lower, a sell the
 * higher); else the book reference alone. Both as { num, den, what } sompi per `scale` base units.
 */
export function bandReference({ side, op, book }) {
  if (op && book) return (side === 'buy' ? lt(book, op) : lt(op, book)) ? book : op;
  return op ?? book ?? null;
}

/**
 * The price band, in exact integers.
 * `price` is sompi per whole token (10^decimals base units), or, with `priceScale`, sompi per
 * `priceScale` base units (an order state's own price). `ref` is { num, den, what }: num / den
 * sompi per `scale` base units (10^min(decimals, 9)), from bandReference. A buy above
 * ref x (1 + band) or a sell below ref x (1 - band) is refused, and so is any price when there is
 * no reference or no decimals (PRICE_BAND_UNCHECKED, R2-T2). `off: true` when the band is off.
 */
export function bandCheck({ side, price, priceScale = null, ref, scale, bandPct, decimals, why = null }) {
  if (bandPct == null) return { ok: true, checked: false, off: true, note: 'price band off' };
  if (!ref || ref.num <= 0n) {
    return { ok: false, checked: false, reason: `PRICE_BAND_UNCHECKED: no usable reference${why ? ` (${why})` : ''}; the operator can set tokens.<covenant id>.refPrice in the config` };
  }
  if (scale == null || decimals == null) return { ok: false, checked: false, reason: "PRICE_BAND_UNCHECKED: the token's decimals are unknown, so the price band cannot be checked" };
  const { num, den, what } = ref;
  const sc = BigInt(scale);
  const whole = 10n ** BigInt(decimals);
  const [pn, pd] = fraction(bandPct);
  // The price per whole token as qn / qd.
  const [qn, qd] = priceScale == null ? [price, 1n] : [price * whole, BigInt(priceScale)];
  const refText = `${kas((num * whole) / (den * sc))} KAS per whole token (${what})`;
  const shown = kas(qn / qd);
  // Compare price with ref x (1 +- pn / (100 pd)). Both sides are multiplied by den x scale x 100
  // pd x qd.
  const lhs = qn * den * sc * 100n * pd;
  const r = num * whole * qd;
  if (side === 'buy' && lhs > r * (100n * pd + pn)) {
    return { ok: false, reason: `PRICE_BAND: a buy at ${shown} KAS is more than ${bandPct}% above the reference ${refText}` };
  }
  if (side === 'sell' && 100n * pd > pn && lhs < r * (100n * pd - pn)) {
    return { ok: false, reason: `PRICE_BAND: a sell at ${shown} KAS is more than ${bandPct}% below the reference ${refText}` };
  }
  return { ok: true, checked: true, note: `within ${bandPct}% of ${refText}` };
}

/** The side of an order state kind on a KAS book: 'sell', 'buy' or null (any other kind). */
export function sideOfKind(kind) {
  const k = String(kind ?? '');
  return /^Kob(Cond|Ifd)?Ask(Kron)?$/.test(k) ? 'sell' : /^Kob(Cond|Ifd)?Bid(Kron)?$/.test(k) ? 'buy' : null;
}

const digits = (v) => (/^\d+$/.test(String(v ?? '')) ? BigInt(v) : null);

/** An order state's limit (price, else take-profit price), sompi per its scale; null if absent. */
export const statePrice = (st) => digits(st?.price ?? st?.tpPrice);

/**
 * The tip caps a planned order breaks (T-1): the tip goes to the executor that fills it, so a
 * large one hands it the order's value. `o` is { kind, state } as the CLI prints it, `value` the
 * KAS it holds (a buy's escrow). Refused: a tip above `maxTipPct` % of the price, or above
 * `maxTip` sompi over the order (a sell's custody, a buy's escrow).
 */
export function tipRefusals(o, value, caps) {
  const side = sideOfKind(o?.kind);
  const st = o?.state ?? {};
  const tip = digits(st.tip) ?? 0n;
  if (!side || tip === 0n) return [];
  const price = statePrice(st);
  const scale = digits(st.scale);
  const out = [];
  if (caps.maxTipPct != null && (price == null || tip * 10_000n > BigInt(Math.round(caps.maxTipPct * 100)) * price)) {
    out.push(`TIP_CAP: the tip ${tip} sompi per ${scale ?? '?'} base units is above ${caps.maxTipPct}% of the price ${price ?? '?'}`);
  }
  if (caps.maxTip != null) {
    // A sell pays tip x amount; a buy's escrow pays price + tip per unit, of which the tip.
    const total = side === 'sell' ? (scale ? (tip * (digits(st.amountLeft) ?? 0n)) / scale : null)
      : price != null ? (big(value) * tip) / (price + tip) : null;
    if (total == null || total > caps.maxTip) {
      out.push(`TIP_CAP: the order's tips come to ${total == null ? 'an unknown amount' : `${kas(total)} KAS`}, above the ${kas(caps.maxTip)} KAS cap (limits.maxTipKas)`);
    }
  }
  return out;
}

export const today = () => utcDay(Date.now());
