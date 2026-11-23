// Files under dataDir: state.json, logs/ (mm-bot.log, actions.jsonl, fills.jsonl, alerts.jsonl) and
// STOP.

import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { initialMode } from './modes.mjs';

export const paths = (dataDir) => ({
  state: join(dataDir, 'state.json'),
  statePrev: join(dataDir, 'state.prev.json'), // the state file before the last save
  check: join(dataDir, 'own-orders.json'), // the bot's own order records, as kob recover reads them
  stop: join(dataDir, 'STOP'),
  pid: join(dataDir, 'mm-bot.pid'),
  log: join(dataDir, 'logs', 'mm-bot.log'),
  actions: join(dataDir, 'logs', 'actions.jsonl'),
  fills: join(dataDir, 'logs', 'fills.jsonl'),
  alerts: join(dataDir, 'logs', 'alerts.jsonl'),
});

export function freshState(nowMs) {
  return {
    version: 1,
    // covenant id -> { side, level, price, amount, quotedFair, placedAt, expiresAt (ms, the --gtd sent), expired,
    // pending: null | { kind, since, txid }, own: { state, prev, txid, tpl } (the order as the bot's own
    // transactions or the node left it, R2-T1), fate: 'unknown' (a placement that may have gone out),
    // idxBad (the indexer does not show it where the node holds it), unconfirmed, cancelSentAt }
    orders: {},
    recent: {}, // ids that ended in the last hour -> { side, level, endedAt } (to attribute late fills)
    lastFillId: null,
    fees: null, // { day, sompi, txs, byKind: { kind: { n, sompi } } }
    feeHistory: [],
    mode: initialMode(nowMs),
    alerts: {},
    retired: {}, // retired token id -> { done, cancels: { order id: sentAt } }
    templates: { cli: null, kinds: {} }, // order kind -> { hash, verified }: verified once the node held an order under it
    foreign: {}, // orders on the bot's key it did not place (R3T-2): id -> { side, price, amountLeft, firstSeen, tries, lastTry }
    blind: [], // placements that may have gone out under no known covenant id: { side, cost, tokens, at }, counted for a day
    placeFails: 0, // placements refused in a row (R2-T3)
    last: null,
  };
}

export function loadState(dataDir, nowMs = Date.now()) {
  const p = paths(dataDir).state;
  if (!existsSync(p)) return freshState(nowMs);
  let s;
  try {
    s = JSON.parse(readFileSync(p, 'utf8'));
  } catch (e) {
    // A fresh state would forget the bot's live quotes, so the operator decides (T-11).
    throw new Error(`${p} does not read (${e.message}): fix it, or replace it with ${paths(dataDir).statePrev} (the state before the last save); the bot does not start without its orders`);
  }
  return { ...freshState(nowMs), ...s };
}

export function saveState(dataDir, state) {
  const p = paths(dataDir).state;
  writeFileSync(`${p}.tmp`, JSON.stringify(state, null, 1) + '\n');
  if (existsSync(p)) copyFileSync(p, paths(dataDir).statePrev);
  renameSync(`${p}.tmp`, p);
}

/** A log past this size moves to <file>.1, replacing the one before (T-11). */
export const LOG_ROTATE_BYTES = 50 * 1024 * 1024;

function rotate(file) {
  try {
    if (statSync(file).size > LOG_ROTATE_BYTES) renameSync(file, `${file}.1`);
  } catch { /* no file yet */ }
}

export class Logger {
  constructor(dataDir, { tag = 'bot', quiet = false } = {}) {
    this.p = paths(dataDir);
    mkdirSync(join(dataDir, 'logs'), { recursive: true });
    this.tag = tag;
    this.quiet = quiet;
  }

  info(msg) {
    const line = `${new Date().toISOString()} ${this.tag} ${msg}`;
    rotate(this.p.log);
    appendFileSync(this.p.log, line + '\n');
    if (!this.quiet) console.log(line);
  }

  action(rec) {
    rotate(this.p.actions);
    appendFileSync(this.p.actions, JSON.stringify({ at: new Date().toISOString(), ...rec }) + '\n');
  }

  alert(rec) {
    appendFileSync(this.p.alerts, JSON.stringify({ ts: new Date().toISOString(), ...rec }) + '\n');
  }

  fill(rec) {
    appendFileSync(this.p.fills, JSON.stringify({ at: new Date().toISOString(), ...rec }) + '\n');
  }
}

/** Adds a paid fee to the day's ledger (rolling the day over at UTC midnight). */
export function addFee(state, kind, sompi, nowMs = Date.now()) {
  const day = new Date(nowMs).toISOString().slice(0, 10);
  if (!state.fees || state.fees.day !== day) {
    if (state.fees) state.feeHistory = [...(state.feeHistory ?? []), state.fees].slice(-60);
    state.fees = { day, sompi: 0, txs: 0, byKind: {} };
  }
  state.fees.sompi += sompi;
  state.fees.txs += 1;
  const k = (state.fees.byKind[kind] ??= { n: 0, sompi: 0 });
  k.n += 1;
  k.sompi += sompi;
}

/** The fees paid today, in sompi (0 after the UTC day rolled over). */
export const feesToday = (state, nowMs = Date.now()) => (state.fees?.day === new Date(nowMs).toISOString().slice(0, 10) ? state.fees.sompi : 0);
