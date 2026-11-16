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

/**
 * The price band: `price` (sompi per whole token) against a reference (the last fill, else the mid of the best bid and
 * ask). A buy above ref x (1 + band) or a sell below ref x (1 - band) is refused. No reference: passes, with a note.
 */
export function bandCheck({ side, price, book, bandPct }) {
  if (bandPct == null) return { ok: true, note: 'price band off' };
  const last = book?.last_fill != null ? BigInt(book.last_fill) : null;
  const bid = book?.best_bid != null ? BigInt(book.best_bid) : null;
  const ask = book?.best_ask != null ? BigInt(book.best_ask) : null;
  const ref = last ?? (bid != null && ask != null ? (bid + ask) / 2n : null);
  if (ref == null || ref <= 0n) return { ok: true, note: 'no last fill and no two-sided book: the price band was not checked' };
  const bps = BigInt(Math.round(Number(bandPct) * 100));
  if (side === 'buy' && price * 10_000n > ref * (10_000n + bps)) {
    return { ok: false, reason: `PRICE_BAND: a buy at ${kas(price)} KAS is more than ${bandPct}% above the reference ${kas(ref)} KAS (${last != null ? 'last fill' : 'mid'})` };
  }
  if (side === 'sell' && price * 10_000n < ref * (10_000n - bps)) {
    return { ok: false, reason: `PRICE_BAND: a sell at ${kas(price)} KAS is more than ${bandPct}% below the reference ${kas(ref)} KAS (${last != null ? 'last fill' : 'mid'})` };
  }
  return { ok: true, note: `within ${bandPct}% of ${kas(ref)} KAS (${last != null ? 'last fill' : 'mid'})` };
}

export const today = () => utcDay(Date.now());
