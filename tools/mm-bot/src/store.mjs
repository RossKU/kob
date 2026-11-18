// Files under dataDir: state.json, logs/ (mm-bot.log, actions.jsonl, fills.jsonl, alerts.jsonl) and STOP.

import { appendFileSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { initialMode } from './modes.mjs';

export const paths = (dataDir) => ({
  state: join(dataDir, 'state.json'),
  stop: join(dataDir, 'STOP'),
  views: join(dataDir, 'order-views.json'), // the last indexer views of the bot's live orders
  pid: join(dataDir, 'mm-bot.pid'),
  log: join(dataDir, 'logs', 'mm-bot.log'),
  actions: join(dataDir, 'logs', 'actions.jsonl'),
  fills: join(dataDir, 'logs', 'fills.jsonl'),
  alerts: join(dataDir, 'logs', 'alerts.jsonl'),
});

export function freshState(nowMs) {
  return {
    version: 1,
    orders: {}, // covenant id -> { side, level, price, amount, quotedFair, placedAt, pending: null | { kind, since, txid } }
    recent: {}, // ids that ended in the last hour -> { side, level, endedAt } (to attribute late fills)
    lastFillId: null,
    fees: null, // { day, sompi, txs, byKind: { kind: { n, sompi } } }
    feeHistory: [],
    mode: initialMode(nowMs),
    alerts: {},
    retired: {}, // retired token id -> { done, cancels: { order id: sentAt } }
    last: null,
  };
}

export function loadState(dataDir, nowMs = Date.now()) {
  const p = paths(dataDir).state;
  if (!existsSync(p)) return freshState(nowMs);
  const s = JSON.parse(readFileSync(p, 'utf8'));
  return { ...freshState(nowMs), ...s };
}

export function saveState(dataDir, state) {
  const p = paths(dataDir).state;
  writeFileSync(`${p}.tmp`, JSON.stringify(state, null, 1) + '\n');
  renameSync(`${p}.tmp`, p);
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
    appendFileSync(this.p.log, line + '\n');
    if (!this.quiet) console.log(line);
  }

  action(rec) {
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
