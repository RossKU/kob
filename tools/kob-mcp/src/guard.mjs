// The safeguards: the spending ledger, the caps, the withdrawal allow-list, the kill switch and the price band.

import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { kas, utcDay } from './units.mjs';

/**
 * A JSON-lines ledger of the KAS each write takes out of the wallet. A send first writes `reserve`; the outcome writes
 * `settle` (the spend the CLI reported) or `release` (nothing was sent). A reserve with neither (a crash, a timeout)
 * keeps counting: unknown outcomes count as spent.
 */
export class Ledger {
  constructor(file) {
    this.file = file;
  }

  entries() {
    if (!existsSync(this.file)) return [];
    return readFileSync(this.file, 'utf8').split(/\r?\n/).filter((l) => l.trim()).map((l) => JSON.parse(l));
  }

  append(e) {
    mkdirSync(dirname(this.file), { recursive: true });
    appendFileSync(this.file, `${JSON.stringify({ ts: new Date().toISOString(), ...e })}\n`);
  }

  reserve(id, day, spend, meta = {}) {
    this.append({ kind: 'reserve', id, day, spend: String(spend), ...meta });
  }

  settle(id, spend, txids) {
    this.append({ kind: 'settle', id, spend: String(spend), txids });
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

  /** Token creations on the UTC day `day` that were not released (sent, or with an unknown outcome). */
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

/**
 * The price band, in exact integers: `price` is sompi per whole token (10^decimals base units); the book's prices are
 * sompi per `book.scale` base units (10^min(decimals, 9)). The reference is the last fill, else the mid of the best bid
 * and ask. A buy above ref x (1 + band) or a sell below ref x (1 - band) is refused. No reference or no decimals: passes,
 * with a note.
 */
export function bandCheck({ side, price, book, bandPct, decimals }) {
  if (bandPct == null) return { ok: true, note: 'price band off' };
  const last = book?.last_fill != null ? BigInt(book.last_fill) : null;
  const bid = book?.best_bid != null ? BigInt(book.best_bid) : null;
  const ask = book?.best_ask != null ? BigInt(book.best_ask) : null;
  // the reference as num / den sompi per `scale` base units (a mid keeps its half sompi)
  const [num, den] = last != null ? [last, 1n] : bid != null && ask != null ? [bid + ask, 2n] : [null, 1n];
  if (num == null || num <= 0n) return { ok: true, note: 'no last fill and no two-sided book: the price band was not checked' };
  if (book?.scale == null || decimals == null) return { ok: true, note: "the token's decimals are unknown: the price band was not checked" };
  const scale = BigInt(book.scale);
  const whole = 10n ** BigInt(decimals);
  const [pn, pd] = fraction(bandPct);
  const what = last != null ? 'last fill' : 'mid';
  const refText = `${kas((num * whole) / (den * scale))} KAS per whole token (${what})`;
  // price ? ref x (1 +- pn / (100 pd)), both sides multiplied by den x scale x 100 pd
  const lhs = price * den * scale * 100n * pd;
  const ref = num * whole;
  if (side === 'buy' && lhs > ref * (100n * pd + pn)) {
    return { ok: false, reason: `PRICE_BAND: a buy at ${kas(price)} KAS is more than ${bandPct}% above the reference ${refText}` };
  }
  if (side === 'sell' && 100n * pd > pn && lhs < ref * (100n * pd - pn)) {
    return { ok: false, reason: `PRICE_BAND: a sell at ${kas(price)} KAS is more than ${bandPct}% below the reference ${refText}` };
  }
  return { ok: true, note: `within ${bandPct}% of ${refText}` };
}

export const today = () => utcDay(Date.now());
