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

/**
 * The price band, in exact integers.
 * `price` is sompi per whole token (10^decimals base units). The book's prices are sompi per
 * `book.scale` base units (10^min(decimals, 9)). The reference is the last fill, else the mid of
 * the best bid and ask. A buy above ref x (1 + band) or a sell below ref x (1 - band) is refused.
 * With no reference or no decimals it passes, with a note.
 */
export function bandCheck({ side, price, book, bandPct, decimals }) {
  if (bandPct == null) return { ok: true, note: 'price band off' };
  const last = book?.last_fill != null ? BigInt(book.last_fill) : null;
  const bid = book?.best_bid != null ? BigInt(book.best_bid) : null;
  const ask = book?.best_ask != null ? BigInt(book.best_ask) : null;
  // The reference as num / den sompi per `scale` base units (a mid keeps its half sompi).
  const [num, den] = last != null ? [last, 1n] : bid != null && ask != null ? [bid + ask, 2n] : [null, 1n];
  if (num == null || num <= 0n) return { ok: true, note: 'no last fill and no two-sided book: the price band was not checked' };
  if (book?.scale == null || decimals == null) return { ok: true, note: "the token's decimals are unknown: the price band was not checked" };
  const scale = BigInt(book.scale);
  const whole = 10n ** BigInt(decimals);
  const [pn, pd] = fraction(bandPct);
  const what = last != null ? 'last fill' : 'mid';
  const refText = `${kas((num * whole) / (den * scale))} KAS per whole token (${what})`;
  // Compare price with ref x (1 +- pn / (100 pd)). Both sides are multiplied by den x scale x 100
  // pd.
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
