// The cycle: reference, wallet and orders, reconcile, mode, actions, funds. Only the operator ends
// the loop.

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { addressOfXOnly } from './address.mjs';
import { readReference } from './feeds.mjs';
import { alertsToSend, fmtAmount, fundChecks } from './funds.mjs';
import { Indexer, indexerUrl } from './indexer.mjs';
import { sideBudgets, skewShift } from './inventory.mjs';
import { gtdArg, Kob } from './kob.mjs';
import { bidCost, buildLadder } from './ladder.mjs';
import { backoffMs, feeGate, IDLE, initialMode, nextMode, QUOTING, sideStatus, STOPPING } from './modes.mjs';
import { planActions, pullAll } from './reprice.mjs';
import { retiredMessage, retiredPlan } from './retired.mjs';
import { inventoryUsd, sizedQuotes } from './sizing.mjs';
import { isJsonWrpc, keyOutputs } from './node.mjs';
import { addFee, feesToday, loadState, Logger, paths, saveState } from './store.mjs';

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const kasOf = (sompi) => sompi / 1e8;
// kob recover's note for an order whose script never changes and has no unspent output (a bid).
const SPENT = /\bspent: no unspent output at the order's script\b/;
const ENDED = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);
const short = (id) => `${id.slice(0, 8)}..${id.slice(-4)}`;
const ERROR_ALERT_AFTER = 3; // consecutive failed cycles before an error alert
const PLACE_FAILS_IDLE = 3; // placements refused in a row before IDLE/placement (R2-T3)
const BLIND_KEEP_MS = 86_400_000; // how long a placement of no known id counts toward the caps
const FOREIGN_TRIES = 3; // cancels tried of an order on the bot's key the bot did not place (R3T-2)
const FOREIGN_PER_CYCLE = 2; // and at most this many of them a cycle (and of a retired token's, R4T-4)
const RETIRED_BUDGET_MS = 20_000; // no retired cancel starts once this much of a cycle went to them
const OWN_TX_KEEP = 2000; // the bot's own transaction ids kept once a node read flagged their outputs (R3T-3, R5T-1)
const OWN_TX_MAX = 20_000; // and kept at most while no node read succeeds
const PAYMENTS_KEEP = 2000; // payments to the bot's address no longer shown on the node, kept ...
const PAYMENT_KEEP_MS = 7 * 86_400_000; // ... for a week after the node last showed them
const FILLS_KEEP = 200; // fill events kept per order
const HINTS_MAX = 32; // amounts left passed to kob recover per ask
// Amounts left derived from the payments the node showed to the bot (R6T-1): from each payment
// first seen after the ask's last known amount, and from sums of 2 and 3 of them. Per ask and
// lookup: at most PAY_SOURCES_MAX payments (C(12,1) + C(12,2) + C(12,3) = 298 sums at most, cut at
// PAY_HINTS_MAX amounts), so with the HINTS_MAX others an ask stays within kob recover's 200
// candidate scripts, the cap every lookup had already. A payment whose filled amount spans more
// than PAY_RANGE_MAX base units (a token whose base unit is worth far less than a sompi) is skipped.
export const PAY_SOURCES_MAX = 12;
export const PAY_HINTS_MAX = 160;
export const PAY_RANGE_MAX = 200;
const CANCEL_TXIDS_KEEP = 8; // txids of the cancels sent per order, any of which may confirm (R6T-2)
// A paid ask is kept as proven (R5T-1): its level and cap share are free, it is still looked up.
export const PROVEN_MAX = 200; // proven records kept; past it a proof frees nothing
// Lookups of proven records a cycle, in rotation: each costs up to 200 candidate scripts in the
// cycle's one getUtxosByAddresses (kob recover), so 5 add at most 1,000 scripts to that call.
export const PROVEN_LOOKUPS = 5;
// When a STOP or an IDLE pull begins, every proven ask gets one lookup, this many a cycle: one kob
// recover --cancel call of up to 4,000 candidate scripts, 80 getUtxosByAddresses requests of 50
// on one connection, well inside cli.timeoutSec (180 s). PROVEN_MAX takes 10 cycles.
export const PROVEN_PASS_BATCH = 20;
const PROVEN_PASS_TRIES = 3; // calls a record is tried in when the node does not answer, then reported not looked up
// A proven record ends this long after its expiry: past it no conforming matcher fills the ask and
// keepers refund it. A record lives at most lifeMin + 2 h (3.5 h at the default 90 min), so
// PROVEN_MAX is reached only above about 57 full fills an hour, sustained.
export const PROVEN_KEEP_AFTER_EXPIRY_MS = 2 * 3_600_000;
const isAsk = (s) => (s.own?.state?.kind ? /Ask/.test(s.own.state.kind) : s.side === 'sell');
/** What a pass over the proven asks did (R5T-1), for the stop line and the log. */
const passText = (p, file) => `proven asks: ${p.looked} of ${p.total} looked up on the node, ${p.cancelled} found live and cancelled${p.found > p.cancelled ? `, ${p.found - p.cancelled} found live and not yet cancelled` : ''}, ${p.notFound.length} not found${p.unasked.length ? `, ${p.unasked.length} not looked up (the node did not answer)` : ''}${p.notFound.length + p.unasked.length ? ` (listed in ${file})` : ''}`;
/** A URL without its user, password or query (they may carry credentials, T-11). */
const shownUrl = (u) => String(u).replace(/\/\/[^@/]*@/, '//').replace(/\?.*$/, '');

/** The plain-English alert for a mode change, with the action needed (null: none). */
function modeMessage(mode, prev, snap, c, feesSompi) {
  const label = c.token.label ?? 'token';
  if (mode.mode === 'STOPPING') return { level: 'info', message: 'Stopping as asked: cancelling every quote, then exiting.' };
  if (mode.mode === 'QUOTING') return { level: 'info', message: 'Quoting both ladders around the fair value.' };
  switch (mode.reason) {
    case 'reference':
      return { level: 'warn', message: `Quotes pulled: no usable KAS/USD reference (${snap.reason}); the bot resumes by itself, no action needed unless it lasts.` };
    case 'jump':
      return { level: 'warn', message: `Quotes pulled: KAS/USD moved ${(snap.jump * 100).toFixed(2)}% within a minute; the bot resumes by itself after a steady ${c.safety.resumeAfterSec} s.` };
    case 'peg':
      return { level: 'warn', message: `Quotes pulled: ${label}/USD is ${snap.tokenUsd.value.toFixed(5)} (${(snap.peg.drift * 100).toFixed(2)}%, band ${(c.token.pegBand * 100).toFixed(2)}%); check the bridge and USDC news, the bot resumes by itself once the rate is back in the band.` };
    case 'placement':
      return { level: 'error', message: `Quotes pulled: the kob CLI refused ${PLACE_FAILS_IDLE} placements in a row (${snap.placeError ?? 'see the log'}); fix the cause (token.decimals, the CLI, the funds), the bot resumes by itself once a planned placement passes.` };
    case 'fee-cap':
      return { level: 'warn', message: `Quotes pulled: today's fees reached ${(feesSompi / 1e8).toFixed(4)} KAS of the ${c.safety.maxFeesKasPerDay} KAS cap; the bot resumes at 00:00 UTC (raise safety.maxFeesKasPerDay if the spend is expected).` };
    case 'cooldown':
      return prev.mode === 'IDLE' && prev.reason !== 'cooldown' ? { level: 'info', message: `The condition cleared (${prev.reason}); quoting resumes after a ${c.safety.resumeAfterSec} s cool-down.` } : null;
    default:
      return null;
  }
}

/**
 * What the bot has put on chain, by the measure of the side caps: tokens in asks (base units) and the
 * escrow of bids (sompi, no carrier). Every tracked order counts, whatever its state, and so does a
 * placement whose id is unknown (R2-T1).
 */
const committed = (orders, blind, scale) => ({
  // A proven ask (R5T-1) no longer counts: the wallet's token balance bounds what can be placed.
  // A cancel-replaced order whose replacement is tracked counts as that replacement (R6T-2).
  askTokens: orders.filter((o) => o.side === 'sell' && !o.proven && !o.replaced).reduce((a, o) => a + o.amountLeft, 0) + blind.filter((b) => b.side === 'sell').reduce((a, b) => a + b.tokens, 0),
  bidKas: orders.filter((o) => o.side === 'buy' && !o.replaced).reduce((a, o) => a + bidCost(o.amountLeft, o.price, scale), 0) + blind.filter((b) => b.side === 'buy').reduce((a, b) => a + b.cost, 0),
});

/** What the bot's own orders hold: tokens in asks (base units), KAS escrow in bids (sompi). */
const holdings = (orders) => ({
  askTokens: orders.filter((o) => o.side === 'sell' && !o.proven && !o.replaced).reduce((a, o) => a + o.amountLeft, 0),
  bidKas: orders.filter((o) => o.side === 'buy' && !o.replaced).reduce((a, o) => a + o.value, 0),
});

export class Bot {
  /** opts: `dryRun` plans with the CLI's --dry-run and never signs. `once` runs one cycle. */
  constructor(cfg, { dryRun = false, once = false } = {}) {
    this.cfg = cfg;
    this.dryRun = dryRun;
    this.once = once;
    this.p = paths(cfg.dataDir);
    this.log = new Logger(cfg.dataDir);
    this.kob = new Kob(cfg, { dryRun });
    this.idx = new Indexer(indexerUrl(cfg));
    this.state = loadState(cfg.dataDir);
    // A stop is final for the run that was stopped, not for the next start.
    if (this.state.mode.mode === STOPPING) this.state.mode = initialMode(Date.now());
    // A pass of an earlier run that did not end goes on; the next STOP or IDLE pull starts its own.
    this.state.provenPassFor = null;
    // `once` skips the start-up cool-down.
    if (once && this.state.mode.reason === 'cooldown') this.state.mode = { ...this.state.mode, steadySince: Date.now() - cfg.safety.resumeAfterSec * 1000 };
    this.refMemory = {};
    this.scale = 10 ** cfg.token.decimals;
    this.stopSignal = false;
    this.failures = 0;
    // The bot key's plain outputs on the node, the proof that a fill paid it (R3T-3). Only a JSON
    // wRPC node can be asked; with the CLI's default or a Borsh node a full fill is never proven.
    this.nodeOutputs = isJsonWrpc(cfg.node) ? (address, key) => keyOutputs(cfg.node, address, key) : null;
  }

  stopRequested() {
    return this.stopSignal || existsSync(this.p.stop);
  }

  save() {
    if (!this.dryRun) saveState(this.cfg.dataDir, this.state);
  }

  /** The loop. Resolves when the operator stopped the bot and its quotes are down. */
  async run() {
    const c = this.cfg;
    const m = this.state.mode;
    this.log.info(`start: ${c.network}, token ${c.token.label ?? ''} ${c.token.covenantId}, indexer ${indexerUrl(c)}, node ${c.node ? shownUrl(c.node) : '(CLI default)'}${this.dryRun ? ', DRY RUN (nothing is signed)' : ''}`);
    if (!this.nodeOutputs) this.log.info('WARNING: node: a JSON wRPC node (ws://host:18110, testnet-10 18210) is needed to prove a fully filled ask paid (without one it stays unresolved and holds its share of the ask cap) and that a cancel confirmed (without one an ask ends once its cancel went out, with no proof that it confirmed) (README, tracking)');
    this.log.info(`mode ${m.mode}${m.reason ? `/${m.reason}` : ''}${m.reason === 'cooldown' ? `: quoting starts once the reference has been fresh and steady for ${c.safety.resumeAfterSec} s` : ''}`);
    for (;;) {
      let ok = false;
      try {
        ok = await this.cycle();
        if (this.failures >= ERROR_ALERT_AFTER) this.alert('info', 'recovered', `Cycles complete again after ${this.failures} failures; no action needed.`);
        this.failures = 0;
      } catch (e) {
        this.failures++;
        this.log.info(`cycle failed (${this.failures} in a row): ${e.stack?.split('\n').slice(0, 3).join(' / ') ?? e.message}`);
        this.log.action({ type: 'error', message: e.message, failures: this.failures });
        if (this.failures === ERROR_ALERT_AFTER) {
          this.alert('error', 'cycle-failing', `The bot cannot complete a cycle (${this.failures} failures in a row: ${e.message}); it keeps retrying, so check the node, the indexer and the kob CLI.`);
        }
      }
      if (ok === 'stopped' || this.once) return;
      const delay = this.failures ? backoffMs(this.failures, c.safety.backoffBaseSec * 1000, c.safety.backoffMaxSec * 1000)
        : this.state.mode.mode === STOPPING ? 5_000 : c.reference.pollSec * 1000;
      if (this.failures) this.log.info(`retry in ${Math.round(delay / 1000)} s`);
      const until = Date.now() + delay;
      while (Date.now() < until && !this.stopRequested()) await wait(Math.min(1000, until - Date.now()));
    }
  }

  /** One cycle. Returns 'stopped' once the bot has stopped. */
  async cycle() {
    const c = this.cfg;
    const now = Date.now();
    const stop = this.stopRequested();
    const snap = stop ? { ok: false, reason: 'stopping', jump: 0, used: [], dropped: [], venues: {} } : await readReference(c, this.refMemory, now);
    if (!stop) this.logReference(snap);

    // A template hash is proven for one CLI build: another build may place under another one.
    if (this.cliVersion === undefined) {
      this.cliVersion = await this.kob.version();
      const t = this.state.templates;
      if (!this.cliVersion || t.cli !== this.cliVersion) this.state.templates = { cli: this.cliVersion, kinds: {} };
    }
    this.state.blind = (this.state.blind ?? []).filter((b) => now - b.at < BLIND_KEEP_MS);
    // An order an older version adopted from the indexer is not the bot's (R3T-2).
    for (const [id, s] of Object.entries(this.state.orders)) {
      if (!s.adopted) continue;
      this.state.foreign[id] = { side: s.side, price: s.price, amountLeft: s.amountLeft, firstSeen: now, tries: 0, lastTry: null };
      delete this.state.orders[id];
    }

    let bal;
    let orders;
    try {
      bal = await this.kob.balances(c.token.covenantId);
      this.maker = bal.maker;
      this.address ??= addressOfXOnly(bal.maker, c.network);
      await this.seePayments(now);
      await this.recordFills();
      orders = await this.reconcile(now);
    } catch (e) {
      // The wallet or the indexer does not answer: a pull that is due goes through the node from
      // the saved views.
      await this.blindPull(stop, snap, now, e);
      throw e;
    }
    if (bal.token.decimals != null && bal.token.decimals !== c.token.decimals) {
      throw new Error(`the CLI reports ${bal.token.decimals} decimals for the token, the config says ${c.token.decimals}: fix token.decimals`);
    }
    if (!stop && snap.ok && this.state.placeFails >= PLACE_FAILS_IDLE) await this.probePlacement(snap, bal, orders);
    snap.placeError = this.placeError;

    const gate = feeGate(feesToday(this.state, now), c.safety);
    const prev = this.state.mode;
    const mode = nextMode(prev, { stop, refOk: snap.ok, jump: snap.jump ?? 0, feeBlocked: !gate.canTrade, pegOk: snap.peg?.ok ?? true,
      placeBlocked: this.state.placeFails >= PLACE_FAILS_IDLE }, c.safety, now);
    if (mode.changed) {
      const why = mode.reason === 'jump' ? ` (moved ${(snap.jump * 100).toFixed(2)}% within a minute)` : mode.reason === 'reference' ? ` (${snap.reason})`
        : mode.reason === 'peg' ? ` (${c.token.label ?? 'token'}/USD ${snap.tokenUsd.value.toFixed(5)}, ${(snap.peg.drift * 100).toFixed(2)}% from the peg)` : '';
      this.log.info(`mode ${prev.mode}${prev.reason ? `/${prev.reason}` : ''} -> ${mode.mode}${mode.reason ? `/${mode.reason}` : ''}${why}`);
      this.log.action({ type: 'mode', from: prev.mode, fromReason: prev.reason, to: mode.mode, reason: mode.reason, detail: why.trim() || null });
      const msg = modeMessage(mode, prev, snap, c, feesToday(this.state, now));
      if (msg) this.alert(msg.level, mode.reason ?? mode.mode.toLowerCase(), msg.message, {}, mode);
    }
    const { changed: _c, pull: _p, ...rec } = mode;
    this.state.mode = rec;
    this.provenPassBegin(mode, now);
    if (this.state.provenPass) await this.provenPassStep(now, { indexer: true });

    let sides = { bids: { quoting: false, reason: 'idle' }, asks: { quoting: false, reason: 'idle' } };
    let plan = null;
    if (mode.pull) {
      // A proven ask is looked up by the pass of this pull (provenPassStep) and by the rotation.
      const toCancel = orders.filter((o) => o.pendingKind !== 'cancel' && o.pendingKind !== 'proven');
      if (toCancel.length) {
        this.log.info(`pull ${toCancel.length} quote(s): ${mode.mode}${mode.reason ? `/${mode.reason}` : ''}`);
        await this.execute(pullAll(toCancel, `pull: ${mode.reason ?? mode.mode}`));
      }
    } else {
      plan = this.plan(snap, bal, orders);
      sides = plan.sides;
      await this.execute(plan.actions);
    }
    if (Object.keys(this.state.foreign).length) await this.cancelForeign(now);

    if (c.retiredTokens.length) await this.retire(bal, mode.mode === STOPPING);
    // A payment that landed while the bot sent is recorded before a later send can spend it.
    if (this.sentSince) await this.seePayments(Date.now());
    const after = this.ordersNow();
    if (snap.ok && !stop) this.checkFunds(snap, bal, orders, plan);
    this.state.last = this.snapshot(snap, bal, orders, after, plan, sides, now);
    this.save();

    if (mode.mode === STOPPING) {
      // An order leaves the state file only once the node shows it ended (reconcile): STOP ends on
      // that, never on what the indexer lists (T-4, R2-T1).
      // An unresolved ask past its own expiry is not waited for (R3T-3): it stays in the state file.
      // A proven ask (paid, R5T-1) is not waited for either, unless the node found it live.
      const parked = Object.values(this.state.orders).filter((s) => s.parked && !s.proven).length;
      const proven = Object.values(this.state.orders).filter((s) => s.proven && !s.proven.liveAt).length;
      const left = Object.values(this.state.orders).filter((s) => (s.proven ? s.proven.liveAt : !s.parked)).length;
      // STOP ends only once its pass has looked up every proven ask (R5T-1).
      const pass = this.state.provenPass;
      if ((left === 0 && !pass) || this.dryRun) {
        const last = this.state.provenPassLast?.why === 'stop' ? this.state.provenPassLast : null;
        this.log.info(`stopped: no quote left${parked ? `; ${parked} unresolved ask(s) past their expiry kept in the state file for the operator` : ''}${last ? `; ${passText(last, this.p.provenNotFound)}` : ''}${proven ? `; ${proven} paid ask(s) kept as proven: the next run keeps looking for them` : ''}`);
        return 'stopped';
      }
      if (pass) this.log.info(`stopping: proven asks: ${pass.looked} of ${pass.total} looked up on the node, ${pass.left.length} to go (${PROVEN_PASS_BATCH} a cycle)`);
      if (left) this.log.info(`stopping: ${left} order(s) not yet shown ended on the node; waiting for the cancels (a cancel not seen in ${c.safety.orderVisibleTimeoutSec} s is sent again)`);
    }
    return true;
  }

  logReference(snap) {
    const v = Object.entries(snap.venues).map(([n, x]) => (x.fresh ? `${n} ${x.vwap.toFixed(6)}` : `${n} -`)).join(', ');
    const dropped = snap.dropped.length ? `; dropped ${snap.dropped.map((d) => `${d.name} (${d.reason})`).join(', ')}` : '';
    if (snap.ok) {
      this.log.info(`reference: KAS/USD ${snap.kasUsd.toFixed(6)} from ${snap.used.join(', ')} [${v}]${dropped}; token USD ${snap.tokenUsd.value.toFixed(5)} (${snap.tokenUsd.source}); `
        + `fair ${snap.fair.toFixed(4)} KAS; 1-min move ${(snap.jump * 100).toFixed(2)}%`);
    } else {
      this.log.info(`reference: none (${snap.reason}) [${v}]${dropped}`);
    }
    if (snap.crossCheck) {
      const rows = snap.crossCheck.rows.map((r) => (r.error ? `${r.venue} - (${r.error})` : `${r.venue} ${r.kasUsd.toFixed(6)} (${(r.deviation * 100).toFixed(2)}%, ${Math.round(r.baseKas)} KAS)`));
      const warn = snap.crossCheck.rows.some((r) => !r.error && Math.abs(r.deviation) > this.cfg.reference.crossCheck.warnDeviation);
      this.log.info(`cross-check KAS/USDC (log only, never sets the price): USDC/USD ${snap.crossCheck.usdcUsd?.toFixed(5) ?? '-'}; ${rows.join(', ')}${warn ? ' WARNING: a USDC book is off the reference' : ''}`);
    }
  }

  /**
   * The reads failed and the mode says pull (STOP, no reference, a jump, the peg, a placement).
   * It cancels the bot's orders through the node from its own records (`kob recover --cancel`), with
   * no indexer.
   */
  async blindPull(stop, snap, now, cause) {
    const c = this.cfg;
    const mode = nextMode(this.state.mode, { stop, refOk: snap.ok, jump: snap.jump ?? 0, feeBlocked: false, pegOk: snap.peg?.ok ?? true,
      placeBlocked: this.state.placeFails >= PLACE_FAILS_IDLE }, c.safety, now);
    // Quotes the bot cannot amend go stale: once fair has moved half the spread from the fair a
    // quote was set at, it would cross fair, so it is pulled too (T-5).
    const drift = snap.ok ? Math.max(0, ...Object.values(this.state.orders).filter((s) => s.quotedFair > 0 && !s.proven).map((s) => Math.abs(snap.fair / s.quotedFair - 1))) : 0;
    const drifted = drift > c.quotes.spreadPct / 200;
    // The pass of a STOP or IDLE pull needs no indexer either.
    this.provenPassBegin(mode, now);
    const passing = !!this.state.provenPass;
    if (passing) await this.provenPassStep(now, { indexer: false });
    if (!mode.pull && !drifted) return;
    const why = mode.pull ? mode.reason ?? mode.mode : `drift: fair moved ${(drift * 100).toFixed(2)}% from a quote's`;
    const timeout = c.safety.orderVisibleTimeoutSec * 1000;
    // Proven asks (R5T-1): the pass's batch above, else this cycle's share of the rotation.
    const look = new Set(passing ? [] : this.provenSlice(now));
    const due = Object.keys(this.state.orders).filter((id) => {
      const s = this.state.orders[id];
      if (s.proven && !s.proven.liveAt && !look.has(id)) return false;
      return s.pending?.kind !== 'cancel' || now - s.pending.since > timeout;
    });
    if (!due.length) return;
    const { changed: _c, pull: _p, ...rec } = mode;
    if (mode.pull) this.state.mode = rec;
    this.log.info(`pull ${due.length} quote(s) without the indexer (${cause.message}): kob recover --cancel from the bot's own records`);
    const res = this.maker ? await this.cancelMany(due) : new Map();
    const sent = [];
    for (const id of due) {
      const r = res.get(id);
      if (!r?.ok) continue;
      sent.push(id);
      if (this.dryRun) continue;
      this.ownTx(r.txid);
      if (r.fee) addFee(this.state, 'cancel', r.fee);
      this.noteCancel(this.state.orders[id], r.txid, now);
    }
    this.log.action({ type: 'blind-cancel', reason: why, ids: sent, txids: sent.map((id) => res.get(id).txid), errors: due.filter((id) => !sent.includes(id)).map((id) => res.get(id)?.error ?? 'no record') });
    const left = due.length - sent.length;
    this.alert(left ? 'error' : 'warn', 'blind-pull', `Quotes pulled without the indexer (${why}): ${sent.length} cancel(s) sent through the node${left ? `; ${left} order(s) could not be cancelled from the bot's records: check them by hand (kob recover)` : ''}.`, {}, mode);
    this.save();
  }

  /** The template an order is looked up under, and whether a live order on the node proved it. */
  tplOf(s) {
    const k = this.state.templates.kinds[s?.own?.state?.kind];
    if (k?.verified) return { hash: k.hash, verified: true };
    return { hash: s?.own?.tpl ?? k?.hash ?? null, verified: false };
  }

  /** A live hit on the node proves the template it was found under, for the order's kind. */
  prove(e) {
    const s = this.state.orders[e.covenant_id];
    const kind = s?.own?.state?.kind;
    if (!kind || !/^[0-9a-f]{64}$/.test(e.template_hash ?? '')) return;
    const k = this.state.templates.kinds[kind];
    if (k?.verified && k.hash === e.template_hash) return;
    this.state.templates.kinds[kind] = { hash: e.template_hash, verified: true };
    this.log.info(`template of ${kind} proven on the node: ${e.template_hash}`);
  }

  /**
   * What the indexer's view adds to the bot's own record: a candidate template hash (proven only
   * by the node), and the whole record for an order the bot placed but has no state of (an older
   * state file).
   */
  learn(s, v) {
    if (!v || !/^[0-9a-f]{64}$/.test(v.template_hash ?? '')) return;
    if (!s.own && v.state_known && v.state?.kind) {
      s.own = { state: v.state, prev: null, txid: v.current?.txid ?? null, tpl: v.template_hash, fromIndexer: true };
    }
    if (!s.own) return;
    s.own.tpl ??= v.template_hash;
    const kind = s.own.state?.kind;
    if (kind && !this.state.templates.kinds[kind]?.verified) this.state.templates.kinds[kind] = { hash: v.template_hash, verified: false };
  }

  /** The record of order `id` as kob recover reads it (an indexer-view shape), or null. */
  recordOf(id, pick = (s) => s.own?.state) {
    const s = this.state.orders[id];
    const st = s && pick(s);
    const { hash } = this.tplOf(s);
    return st?.kind && hash ? { covenant_id: id, template_hash: hash, status: 'open', state_known: true, state: st, extension_commitment: st.state?.extensionCommitment ?? null } : null;
  }

  /** The records of `ids` as kob recover reads them, written to one file (own-orders.json). */
  writeRecords(ids, pick = (s) => s.own?.state, file = this.p.check) {
    const items = ids.map((id) => this.recordOf(id, pick)).filter(Boolean);
    if (!items.length) return null;
    writeFileSync(file, `${JSON.stringify({ items })}\n`);
    return items.map((i) => i.covenant_id);
  }

  /**
   * The node's word on each order, from the bot's own record (R2-T1): { status: 'live', txid,
   * amountLeft, value, prev } | { status: 'gone', verified } | { status: 'foreign' } | { status:
   * 'unknown', why }. An order whose amend was not seen is looked up under its state before the
   * amend as well.
   */
  async nodeCheck(ids, hints = {}) {
    const out = new Map(ids.map((id) => [id, { status: 'unknown', why: 'no record or template to look it up by' }]));
    const pass = async (list, pick, prev) => {
      const sent = this.writeRecords(list, pick);
      if (!sent) return;
      const r = await this.kob.recover(this.p.check, { maker: this.maker, hints: Object.fromEntries(sent.filter((id) => hints[id]).map((id) => [id, hints[id]])) });
      if (!r.ok) {
        for (const id of sent) out.set(id, { status: 'unknown', why: r.error });
        return;
      }
      for (const e of r.orders) if (e.status === 'live') this.prove(e);
      for (const e of r.orders) {
        const id = e.covenant_id;
        if (!sent.includes(id)) continue;
        const t = this.tplOf(this.state.orders[id]);
        if (e.status === 'live') out.set(id, { status: 'live', txid: String(e.outpoint ?? '').split(':')[0], amountLeft: e.amount_left ?? null, value: Number(e.value ?? 0), prev });
        else if (e.status === 'not_found') out.set(id, { status: 'gone', verified: t.verified && t.hash === e.template_hash, spent: SPENT.test(e.note ?? '') });
        else if (e.status === 'other_maker') out.set(id, { status: 'foreign' });
        else out.set(id, { status: 'unknown', why: `${e.status}: ${e.note ?? ''}` });
      }
    };
    await pass(ids, (s) => s.own?.state, false);
    const back = ids.filter((id) => out.get(id).status === 'gone' && this.state.orders[id]?.own?.prev);
    if (back.length) {
      const first = new Map(back.map((id) => [id, out.get(id)]));
      await pass(back, (s) => s.own.prev, true);
      for (const id of back) if (out.get(id).status !== 'live') out.set(id, first.get(id));
    }
    return out;
  }

  /**
   * Cancels through the node from the bot's own records (kob recover --cancel), never through the
   * indexer's listing (R2-T1). Returns Map id -> { ok, txid, fee, live, error }.
   */
  async cancelMany(ids) {
    const out = new Map(ids.map((id) => [id, { ok: false, live: null, error: 'no record or template to cancel it by' }]));
    const s0 = (id) => this.state.orders[id];
    const withPrev = ids.flatMap((id) => (s0(id)?.own?.prev ? [id] : []));
    const run = async (list, pick) => {
      const sent = this.writeRecords(list, pick);
      if (!sent) return;
      const hints = Object.fromEntries(sent.filter((id) => isAsk(s0(id))).map((id) => [id, this.hintsOf(id)]));
      const r = await this.kob.recover(this.p.check, { maker: this.maker, hints, cancel: true });
      if (!r.ok) {
        for (const id of sent) if (!out.get(id).ok) out.set(id, { ok: false, live: null, error: r.error });
        return;
      }
      for (const e of r.orders) {
        if (!sent.includes(e.covenant_id) || out.get(e.covenant_id).ok) continue;
        if (e.status === 'live') this.prove(e);
        if (e.status === 'live' && isAsk(s0(e.covenant_id)) && e.amount_left != null) this.markPayHit(e.covenant_id, e.amount_left);
        const done = e.cancel?.status === 'submitted' || (this.dryRun && e.cancel?.status === 'signed');
        out.set(e.covenant_id, done ? { ok: true, dryRun: this.dryRun, txid: e.cancel.txid ?? null, fee: Number(e.cancel.fee ?? 0) }
          : { ok: false, live: e.status === 'live', error: e.status === 'live' ? `kob recover --cancel: ${e.cancel?.status ?? '?'}: ${e.cancel?.reason ?? r.error ?? ''}` : `not found on the node (${e.status})` });
      }
    };
    await run(ids, (s) => s.own?.state);
    const again = withPrev.filter((id) => !out.get(id).ok && out.get(id).live === false);
    if (again.length) await run(again, (s) => s.own.prev);
    return out;
  }

  /**
   * Every amount left the bot knows of an ask, for kob recover's --amount-left (R3T-3): its last
   * record, the indexer's list and per-order view, and the placed amount less the fills the
   * indexer reported. The node checks each; none is trusted.
   */
  hintsOf(id, listed = null, { pay = true } = {}) {
    const base = this.indexerHints(id, listed);
    if (!pay) return base;
    const p = this.payHints(id);
    return [...new Set([...base, ...p])];
  }

  /**
   * Amounts left of ask `id` that owe nothing to the indexer (R6T-1), from the payments the node
   * showed to the bot. A fill of n base units pays the maker, at the ask's own output, at least
   * allIn = ceil(n x (price - tip) / scale) (KobAsk.sil, and its KRON form, the `allIn` line); a
   * partial fill leaves both carriers on the order, so a matcher's payout is exactly allIn. A
   * payment P then names n in [floor((P - 1) x scale / r) + 1, floor(P x scale / r)], r = price -
   * tip: one value while a base unit is worth a sompi or more, about scale / r otherwise. The
   * payments are those first seen after the ask's last known amount (its own outpoint's, or the
   * node's last live answer), not the bot's own, and not used to prove another ask; the ones the
   * indexer names as fills of this ask go first. A hit marks its payments used by the ask
   * (markPayHit). A payout above allIn (a filler who pays the maker more) names a larger n than
   * was filled, and is then no help: README, tracking.
   */
  payHints(id) {
    const s = this.state.orders[id];
    (this.paySrc ??= {})[id] = new Map();
    if (!s || !isAsk(s) || !this.nodeOutputs) return [];
    const base = s.leftAt != null && (s.ownAt == null || s.leftAt >= s.ownAt) ? { left: s.amountLeft, at: s.leftAt } : { left: s.ownAmount ?? s.amountLeft ?? s.amount, at: s.ownAt };
    if (!Number.isSafeInteger(base.left) || base.left < 2 || base.at == null) return [];
    const st = s.own?.state?.state ?? {};
    let r;
    let sc;
    let minFill;
    try {
      r = BigInt(st.price ?? Math.round(s.price ?? 0)) - BigInt(st.tip ?? 0);
      sc = BigInt(st.scale ?? this.scale);
      minFill = BigInt(st.minFill ?? 1);
    } catch {
      return [];
    }
    if (r <= 0n || sc <= 0n) return [];
    const own = new Set(this.state.ownTx ?? []);
    const named = new Set((s.fills ?? []).map((f) => f.txid));
    const left = BigInt(base.left);
    const ranges = [];
    const wide = [];
    const pays = Object.entries(this.state.payments)
      .filter(([, p]) => !p.own && !own.has(p.txid) && (!p.usedBy || p.usedBy === id) && p.at > base.at && Number.isSafeInteger(p.sompi) && p.sompi > 0)
      .sort((a, b) => Number(named.has(b[1].txid)) - Number(named.has(a[1].txid)) || a[1].at - b[1].at);
    for (const [k, p] of pays) {
      if (ranges.length >= PAY_SOURCES_MAX) break;
      const P = BigInt(p.sompi);
      let hi = (P * sc) / r;
      let lo = ((P - 1n) * sc) / r + 1n;
      if (hi - lo + 1n > BigInt(PAY_RANGE_MAX)) {
        wide.push(k);
        continue;
      }
      if (lo > hi) lo = hi; // more than allIn of any n: the largest n it covers
      if (lo < minFill) lo = minFill;
      if (lo >= left) continue; // more than a partial fill of this ask leaves
      if (hi >= left) hi = left - 1n;
      if (lo <= hi) ranges.push({ k, lo, hi });
    }
    if (wide.length) {
      const fresh = wide.filter((k) => !(this.wideLogged ??= new Set()).has(`${id}/${k}`));
      for (const k of fresh) this.wideLogged.add(`${id}/${k}`);
      if (fresh.length) this.log.info(`${short(id)}: ${fresh.length} payment(s) to the bot not used as lookup hints: each could pay for more than ${PAY_RANGE_MAX} different fill amounts (a base unit worth far less than a sompi)`);
    }
    const src = this.paySrc[id];
    const combos = [];
    for (let i = 0; i < ranges.length; i++) {
      combos.push([i]);
    }
    for (let i = 0; i < ranges.length; i++) for (let j = i + 1; j < ranges.length; j++) combos.push([i, j]);
    for (let i = 0; i < ranges.length; i++) for (let j = i + 1; j < ranges.length; j++) for (let m = j + 1; m < ranges.length; m++) combos.push([i, j, m]);
    for (const c of combos) {
      const lo = c.reduce((a, i) => a + ranges[i].lo, 0n);
      const hi = c.reduce((a, i) => a + ranges[i].hi, 0n);
      for (let n = hi; n >= lo && src.size < PAY_HINTS_MAX; n--) {
        const rest = Number(left - n);
        if (rest >= 1 && !src.has(rest)) src.set(rest, c.map((i) => ranges[i].k));
      }
      if (src.size >= PAY_HINTS_MAX) break;
    }
    return [...src.keys()];
  }

  /** A lookup found ask `id` at `amountLeft`: the payments that hint named are its fills (R6T-1). */
  markPayHit(id, amountLeft) {
    const keys = this.paySrc?.[id]?.get(Number(amountLeft));
    const s = this.state.orders[id];
    if (!keys || !s || Number(amountLeft) === s.amountLeft) return;
    for (const k of keys) if (this.state.payments[k]) this.state.payments[k].usedBy = id;
    this.log.info(`${short(id)}: found on the node at amount left ${amountLeft}, from the payment(s) ${keys.join(', ')} to the bot (not from the indexer): they are marked as its fills`);
  }

  /** The hints of hintsOf that come from the bot's own records and the indexer. */
  indexerHints(id, listed = null) {
    const s = this.state.orders[id];
    const all = [s?.amountLeft, listed, s?.idxAmount, s?.amount != null && s?.filledSeen ? s.amount - s.filledSeen : null];
    // The amount left after each fill the indexer reported (R5T-1): from the last own outpoint
    // over the fills first seen after it, and from the placed amount over all of them. A lie
    // about one fill then still leaves the amount after the true ones before it among the hints.
    const after = (base, fills) => {
      let left = base;
      return (fills ?? []).map((f) => (left -= f.amount));
    };
    if (s?.fills?.length) {
      if (s.ownAmount != null) all.push(...after(s.ownAmount, s.fills.filter((f) => f.at == null || s.ownAt == null || f.at > s.ownAt)));
      if (s.amount != null) all.push(...after(s.amount, s.fills));
    }
    return [...new Set(all.filter((n) => Number.isSafeInteger(n) && n > 0))].slice(0, HINTS_MAX);
  }

  /**
   * Records a cancel of `s` that went out: pending until seen, with its txid kept beside
   * cancelSentAt, so that the order ends only once the node shows that transaction (R6T-2).
   */
  noteCancel(s, txid, now) {
    s.pending = { kind: 'cancel', since: now, txid: txid ?? null };
    s.cancelSentAt = now;
    if (/^[0-9a-f]{64}$/.test(txid ?? '')) s.cancelTxids = [...(s.cancelTxids ?? []), txid].slice(-CANCEL_TXIDS_KEEP);
  }

  /**
   * Whether the node showed that a cancel of `s` the bot sent confirmed (R6T-2): an output of the
   * cancel's own transaction to the bot's address (the order's carrier comes back as change), or,
   * for a cancel-replace, the replacement live on the node. A submitted cancel is no proof: a fill
   * that lands first leaves the ask live at an amount the search may not reach. Without a JSON
   * wRPC node (testnet-10 only) the bot cannot see its outputs, and the cancel going out is taken.
   */
  cancelConfirmed(s) {
    if (s.pending?.kind !== 'cancel' && s.cancelSentAt == null) return false;
    if (!this.nodeOutputs || s.replaceSeen) return true;
    const txs = new Set([...(s.cancelTxids ?? []), s.pending?.kind === 'cancel' ? s.pending.txid : null].filter(Boolean));
    return Object.values(this.state.payments).some((p) => txs.has(p.txid));
  }

  /** Whether the node showed an output of a placement of unknown outcome: it went out (R6T-2). */
  wentOut(s) {
    const t = s.pending?.kind === 'place' ? s.pending.txid : null;
    return !!t && !!this.nodeOutputs && Object.values(this.state.payments).some((p) => p.txid === t);
  }

  /** One cancel through the node from the bot's record only (kob recover --cancel). */
  async cancelNode(id) {
    const r = (await this.cancelMany([id])).get(id);
    return r.ok ? r : { ok: false, error: r.error };
  }

  /**
   * Starts the pass over every proven ask when a STOP or an IDLE pull begins (R5T-1): one STOP pass
   * a run, one IDLE pass each time quoting stops; back to QUOTING clears it.
   */
  provenPassBegin(mode, now) {
    if (!mode.pull) {
      this.state.provenPassFor = null;
      return;
    }
    const stop = mode.mode === STOPPING;
    if (stop ? this.state.provenPassFor === 'stop' : mode.reason === 'cooldown' || this.state.provenPassFor != null) return;
    const why = stop ? 'stop' : `idle/${mode.reason}`;
    this.state.provenPassFor = why;
    const ids = Object.entries(this.state.orders).filter(([, s]) => s.proven && !s.proven.liveAt).map(([id]) => id);
    if (!ids.length) {
      this.state.provenPass = null;
      this.state.provenPassLast = { why, at: now, total: 0, looked: 0, cancelled: 0, found: 0, notFound: [], unasked: [] };
      return;
    }
    this.state.provenPass = { why, at: now, total: ids.length, left: ids, looked: 0, cancelled: 0, found: 0, notFound: [], unasked: [], tries: {} };
    this.log.info(`${stop ? 'STOP' : `IDLE/${mode.reason}`}: every proven ask (${ids.length}) is looked up on the node once, ${PROVEN_PASS_BATCH} a cycle, and cancelled if found`);
  }

  /**
   * One batch of the pass: kob recover --cancel from the records, with every hint (the indexer's
   * per-order view refreshes its amount when it answers). Never kob cancel through the indexer.
   */
  async provenPassStep(now, { indexer }) {
    const pass = this.state.provenPass;
    const batch = [];
    while (batch.length < PROVEN_PASS_BATCH && pass.left.length) {
      const id = pass.left.shift();
      const s = this.state.orders[id];
      if (s?.proven && !s.proven.liveAt) batch.push(id);
      else if (s) { pass.looked++; pass.found++; } // found live by the rotation since
    }
    if (batch.length) {
      if (indexer) {
        for (const id of batch) {
          const v = await this.idx.order(id).catch(() => null);
          if (v?.amountLeft > 0) this.state.orders[id].idxAmount = v.amountLeft;
        }
      }
      const res = this.maker ? await this.cancelMany(batch) : new Map();
      for (const id of batch) {
        const s = this.state.orders[id];
        const r = res.get(id);
        s.proven.lookedAt = now;
        if (r?.ok || r?.live === true) {
          pass.looked++;
          pass.found++;
          s.proven.liveAt = now;
          if (r.ok) pass.cancelled++;
          this.alert('error', 'proven-live', `Order ${short(id)} (${s.side} L${s.level}) was taken as paid in full, but the node holds it live: ${r.ok ? `cancelled through the node (tx ${r.txid ?? '-'})` : `its cancel failed (${r.error}); it is tried again`}; the fills the indexer reported for it were wrong, so check the indexer.`);
          if (r.ok && !this.dryRun) {
            this.ownTx(r.txid);
            if (r.fee) addFee(this.state, 'cancel', r.fee);
            this.noteCancel(s, r.txid, now);
          }
        } else if (r?.live === false) {
          pass.looked++;
          pass.notFound.push(id);
        } else {
          // The node did not answer: tried again, then reported as not looked up.
          pass.tries[id] = (pass.tries[id] ?? 0) + 1;
          if (pass.tries[id] < PROVEN_PASS_TRIES) pass.left.push(id);
          else pass.unasked.push(id);
        }
      }
    }
    if (pass.left.length) return;
    const { tries: _t, left: _l, ...last } = pass;
    this.state.provenPass = null;
    this.state.provenPassLast = { ...last, done: now };
    const open = [...last.notFound, ...last.unasked].filter((id) => this.state.orders[id]);
    if (open.length) this.writeRecords(open, undefined, this.p.provenNotFound);
    this.log.info(`${last.why === 'stop' ? 'STOP' : last.why.toUpperCase()}: ${passText(last, this.p.provenNotFound)}`);
    this.log.action({ type: 'proven-pass', ...last, file: open.length ? this.p.provenNotFound : null });
  }

  /**
   * The proven asks to look up this cycle (R5T-1): at most PROVEN_LOOKUPS, the ones the indexer
   * lists first (at most 2 of them), then the ones looked up least recently. Marks them looked up.
   */
  provenSlice(now, listed = new Set()) {
    const all = Object.entries(this.state.orders).filter(([, s]) => s.proven && !s.proven.liveAt)
      .sort((a, b) => (a[1].proven.lookedAt ?? 0) - (b[1].proven.lookedAt ?? 0)).map(([id]) => id);
    const first = all.filter((id) => listed.has(id)).slice(0, 2);
    const pick = [...first, ...all.filter((id) => !first.includes(id))].slice(0, PROVEN_LOOKUPS);
    for (const id of pick) this.state.orders[id].proven.lookedAt = now;
    return pick;
  }

  /** Ends the proven asks past their expiry by PROVEN_KEEP_AFTER_EXPIRY_MS (R5T-1). */
  endProven(now) {
    const life = this.life().lifeMs;
    for (const [id, s] of Object.entries(this.state.orders)) {
      if (!s.proven || s.proven.liveAt) continue;
      // An unknown expiry: the earlier terms were set at most lifeMin before the proof.
      const expiry = s.expiresAt ?? s.proven.at + life;
      if (now < expiry + PROVEN_KEEP_AFTER_EXPIRY_MS) continue;
      this.log.info(`${short(id)} (${s.side} L${s.level}): proven paid and not found on the node for ${PROVEN_KEEP_AFTER_EXPIRY_MS / 3_600_000} h past its expiry: ended (no conforming matcher fills an expired ask, and keepers refund it)`);
      this.log.action({ type: 'ended', id, side: s.side, level: s.level, status: 'proven, past its expiry' });
      this.state.recent[id] = { side: s.side, level: s.level, endedAt: now };
      delete this.state.orders[id];
    }
  }

  /**
   * One cancel: through the node from the bot's record; when the record finds nothing, through the
   * indexer's view (kob cancel <id>, which the CLI checks on the node), as for an order partly
   * filled by an amount the record cannot follow.
   */
  async cancelOrder(id) {
    // A proven ask is cancelled through the node only, never from the indexer's view (R5T-1).
    if (this.state.orders[id]?.proven) return this.cancelNode(id);
    const r = (await this.cancelMany([id])).get(id);
    if (r.ok || r.live) return r.ok ? r : { ok: false, error: r.error };
    return this.kob.cancel(id);
  }

  /**
   * Syncs the state file with the node and returns the live orders (R2-T1). The bot tracks each
   * order by the outpoint its own place or amend produced. The indexer only says where to look:
   * an order it does not list, lists at another outpoint, or that has an open question is looked
   * up on the node from the bot's record. An order leaves the state file only when the node finds
   * no unspent output of it under a proven template, and the indexer does not list it or the bot
   * cancelled it. An order of the token on the bot's key that the bot did not place is not
   * adopted (R3T-2).
   */
  async reconcile(now) {
    const c = this.cfg;
    const live = await this.idx.liveOrders(this.maker, c.token.covenantId);
    const raw = new Map((this.idx.views[c.token.covenantId] ?? []).map((v) => [v.covenant_id, v]));
    const byId = new Map(live.map((o) => [o.id, o]));
    const timeout = c.safety.orderVisibleTimeoutSec * 1000;
    const due = (p) => !p || now - p.since > timeout;
    const check = [];
    const hints = {};
    this.endProven(now);
    // Proven asks (R5T-1) are looked up a few a cycle, in rotation, the ones listed first.
    const look = this.provenSlice(now, new Set(Object.keys(this.state.orders).filter((id) => byId.has(id))));
    for (const id of look) {
      const s = this.state.orders[id];
      this.learn(s, raw.get(id));
      check.push(id);
      if (!byId.has(id)) {
        const v = await this.idx.order(id).catch(() => null);
        if (v?.amountLeft > 0) s.idxAmount = v.amountLeft;
      }
      hints[id] = this.hintsOf(id, byId.get(id)?.amountLeft ?? null);
    }
    for (const [id, s] of Object.entries(this.state.orders)) {
      if (s.proven && !s.proven.liveAt) continue;
      const o = byId.get(id);
      this.learn(s, raw.get(id));
      if (!o) {
        if (s.pending?.kind === 'place' && !due(s.pending)) continue;
        check.push(id);
        // An ask the list leaves out: the indexer's own view of it may still name its amount, which
        // the node then checks (R3T-3).
        if (isAsk(s)) {
          const v = await this.idx.order(id).catch(() => null);
          if (v?.amountLeft > 0) s.idxAmount = v.amountLeft;
          hints[id] = this.hintsOf(id);
        }
        continue;
      }
      if (isAsk(s)) hints[id] = this.hintsOf(id, o.amountLeft);
      // The expiry is the bot's own record; the indexer's flag only fills a gap (T-8).
      if (s.expiresAt == null) s.expired = o.expired;
      const atOwn = s.own?.txid == null || o.txid === s.own.txid;
      if (s.pending && s.pending.kind !== 'cancel') {
        const seen = s.pending.kind === 'amend' ? o.txid === s.pending.txid : atOwn;
        if (seen) {
          s.pending = null;
          if (s.own) s.own.prev = null;
        } else {
          if (due(s.pending)) check.push(id);
          continue;
        }
      } else if (s.pending?.kind === 'cancel' && !due(s.pending)) continue;
      if (!atOwn || s.pending || s.idxBad || s.fate || s.unconfirmed) check.push(id);
    }
    // A template the indexer named is proven once on the node, on an order it shows where the bot
    // put it, so that a filled or cancelled order can end later.
    for (const [kind, k] of Object.entries(this.state.templates.kinds)) {
      if (k.verified) continue;
      const one = Object.entries(this.state.orders).find(([id, s]) => s.own?.state?.kind === kind && !s.proven && !check.includes(id));
      if (one) check.push(one[0]);
    }
    // A cancel-replaced record due for its check: its replacement is looked up too, and live on
    // the node it proves that the replace confirmed (R6T-2).
    const replaced = check.filter((id) => this.state.orders[this.state.orders[id]?.replacedBy]);
    for (const id of replaced) if (!check.includes(this.state.orders[id].replacedBy)) check.push(this.state.orders[id].replacedBy);
    if (check.length) {
      const res = await this.nodeCheck(check, hints);
      for (const id of replaced) if (res.get(this.state.orders[id].replacedBy)?.status === 'live') this.state.orders[id].replaceSeen = true;
      for (const id of check) if (this.state.orders[id]) await this.settle(id, res.get(id), byId.get(id), now);
    }
    for (const [id, r] of Object.entries(this.state.recent)) if (now - r.endedAt > 3_600_000) delete this.state.recent[id];

    // An order of the token on the bot's key that the bot did not place: anyone can make one, with
    // any price and amount and no custody (R3T-2). It is not the bot's: it counts toward no cap
    // and no self-trade check, and is never planned. Its cancel is tried a few times (it may be a
    // quote of a lost state file), and one alert names the new ones.
    const seen = new Set();
    const fresh = [];
    for (const o of live) {
      if (this.state.orders[o.id] || this.state.recent[o.id] || !o.side || (o.contract !== 'KobAsk' && o.contract !== 'KobBid')) continue;
      seen.add(o.id);
      if (this.state.foreign[o.id]) continue;
      this.log.info(`${short(o.id)}: a live ${o.side} of this token on the bot's key that the bot did not place: not counted as the bot's; its cancel is tried at most ${FOREIGN_TRIES} times`);
      this.log.action({ type: 'unknown-order', id: o.id, side: o.side, price: o.price, amountLeft: o.amountLeft });
      this.state.foreign[o.id] = { side: o.side, price: o.price, amountLeft: o.amountLeft, firstSeen: now, tries: 0, lastTry: null };
      fresh.push(o.id);
    }
    if (!this.idx.cut[c.token.covenantId]) for (const id of Object.keys(this.state.foreign)) if (!seen.has(id)) delete this.state.foreign[id];
    if (fresh.length) {
      this.alert('warn', 'foreign-order', `${fresh.length} order(s) of the token on the bot's key that the bot did not place (${fresh.map(short).join(', ')}; ${Object.keys(this.state.foreign).length} in all): ignored for the caps and the self-trade check; a cancel is tried at most ${FOREIGN_TRIES} times. Anyone can make such an order; if one is yours, cancel it by hand (kob cancel <id>).`);
    }
    return this.ordersNow();
  }

  /** A few cancels of the orders on the bot's key it did not place, each tried at most FOREIGN_TRIES times (R3T-2). */
  async cancelForeign(now) {
    const gap = this.cfg.safety.orderVisibleTimeoutSec * 1000;
    const due = Object.entries(this.state.foreign).filter(([, f]) => f.tries < FOREIGN_TRIES && (f.lastTry == null || now - f.lastTry > gap)).slice(0, FOREIGN_PER_CYCLE);
    for (const [id, f] of due) {
      const r = await this.kob.cancel(id);
      f.tries += 1;
      f.lastTry = now;
      this.log.action({ type: 'foreign-cancel', id, status: r.ok ? (r.dryRun ? 'planned' : 'submitted') : 'failed', txid: r.txid ?? null, fee: r.fee ?? null, error: r.error ?? null, tries: f.tries });
      this.log.info(`${short(id)} (not placed by the bot): cancel ${r.ok ? `${r.dryRun ? 'planned' : 'submitted'} tx ${r.txid ?? '-'}` : `FAILED ${r.error}`}${!r.ok && f.tries >= FOREIGN_TRIES ? '; not tried again' : ''}`);
      if (r.ok && !r.dryRun) {
        this.ownTx(r.txid);
        addFee(this.state, 'cancel', r.fee);
        f.tries = FOREIGN_TRIES;
      }
    }
  }

  /** Applies the node's word on one order (reconcile). */
  async settle(id, r, o, now) {
    const s = this.state.orders[id];
    const timeout = this.cfg.safety.orderVisibleTimeoutSec * 1000;
    const tag = `${short(id)} (${s.side} L${s.level})`;
    if (r.status === 'live') {
      if (r.prev) {
        // The amend never landed: the order sits under its state before it.
        this.log.info(`${tag}: the amend ${s.pending?.txid ?? ''} did not land; the order is at its earlier terms`);
        s.own.state = s.own.prev;
        s.price = Number(s.own.state.state?.price ?? s.price);
        if (/^\d+$/.test(String(s.own.state.state?.amountLeft ?? ''))) s.ownAmount = Number(s.own.state.state.amountLeft);
        s.quotedFair = 0; // unknown quote: the next cycle amends it to the target
        s.expiresAt = null; // and renews its expiry
      }
      if (s.own) Object.assign(s.own, { prev: null, txid: r.txid });
      s.value = r.value;
      if (r.amountLeft != null && /^\d+$/.test(String(r.amountLeft))) {
        if (isAsk(s)) this.markPayHit(id, r.amountLeft);
        s.amountLeft = Number(r.amountLeft);
        s.leftAt = now; // the node's last word on its amount (payHints)
        if (isAsk(s) && s.own?.state?.state) s.own.state.state.amountLeft = String(r.amountLeft);
      }
      if (s.fate) this.log.info(`${tag}: the placement whose outcome was unknown is on chain`);
      s.fate = null;
      s.cancelUnseen = false;
      if (s.replacedBy) {
        // Its cancel-replace never landed (it spends this very output): the replacement record is
        // a placement that may not have gone out, and this order is tracked again (R6T-2).
        const n = this.state.orders[s.replacedBy];
        this.log.info(`${tag}: its cancel-replace ${s.pending?.txid ?? ''} did not land: the order is live at ${r.txid}`);
        if (n) n.fate = 'unknown';
        s.replacedBy = null;
      }
      s.unconfirmed = false;
      if (s.pending?.kind === 'cancel' ? now - s.pending.since > timeout : s.pending) s.pending = null; // an unseen cancel is sent again
      // A proven ask the node holds live (R5T-1: a proof the indexer forged, or a fill it named
      // wrong): its level went to a new quote, so it counts again and is cancelled.
      if (s.proven) {
        if (!s.proven.liveAt) {
          this.alert('error', 'proven-live', `Order ${tag} was taken as paid in full, but the node holds it live at ${r.txid} (amount left ${r.amountLeft ?? '?'}): it is cancelled through the node; the fills the indexer reported for it were wrong, so check the indexer.`);
          s.proven.liveAt = now;
        }
        if (s.pending?.kind !== 'cancel') await this.run1({ kind: 'cancel', id, side: s.side, level: s.level, reason: 'a proven ask the node holds live' });
        return;
      }
      // A list cut at its page cap may leave an older order out: no proof that the indexer hides it (R4T-3).
      const why = !o ? (this.idx.cut[this.cfg.token.covenantId] ? null : 'does not list it') : o.txid !== r.txid ? `lists it at another outpoint (${o.txid})` : null;
      if (!why || s.pending?.kind === 'cancel') {
        s.idxBad = false;
        return;
      }
      if (!s.idxBad) this.alert('error', 'indexer-hides', `The indexer ${why} for order ${tag}, but the node holds it at ${r.txid}: it is cancelled through the node; check the indexer.`);
      s.idxBad = true;
      if (s.pending?.kind !== 'cancel') await this.run1({ kind: 'cancel', id, side: s.side, level: s.level, reason: 'the indexer does not show it where the node holds it' });
      return;
    }
    if (r.status === 'gone' || r.status === 'foreign') {
      const sent = s.pending?.kind === 'cancel' || s.cancelSentAt != null;
      let cancelled = this.cancelConfirmed(s);
      // The cancel may have confirmed after this cycle's read of the payments: one more read a
      // cycle, after the lookup, so that a cancel the lookup saw land is not taken for a lost one.
      if (!cancelled && sent && r.status === 'gone' && r.verified && !r.spent && this.nodeOutputs && this.paymentsReread !== now) {
        this.paymentsReread = now;
        if (await this.seePayments(now)) cancelled = this.cancelConfirmed(s);
      }
      // A placement of unknown fate whose transaction shows an output on the node went out: from
      // now on it is an order like any other (R6T-2).
      if (s.fate && this.wentOut(s)) {
        this.log.info(`${tag}: the placement whose outcome was unknown went out (the node shows an output of ${s.pending.txid})`);
        s.fate = null;
      }
      // Proof that it ended: no output at a script that never changes (a bid), the bot's own
      // cancel confirmed on the node (R6T-2), or a placement of unknown fate that neither the node
      // nor the indexer shows. An ask the node does not find may sit at an amount the search did
      // not reach (a fill of any amount), so that alone is no proof: it is kept as unresolved
      // (R3T-3). So is one whose cancel went out but never showed on the node: a fill that landed
      // first leaves it live (R6T-2); it is looked up, and cancelled again once found.
      const ended = r.status === 'gone' && r.verified && (r.spent || cancelled || (s.fate && !o));
      if (!ended && sent && !s.cancelUnseen && r.status === 'gone' && r.verified) {
        s.cancelUnseen = true;
        this.log.info(`${tag}: not found on the node, but no cancel the bot sent (${(s.cancelTxids ?? []).map(short).join(', ') || '-'}) shows on it: not taken as cancelled (a fill may have landed first)`);
      }
      if (ended) {
        const status = s.fate ? 'the placement never went out' : cancelled ? 'cancelled (its cancel shows on the node)' : 'spent on the node (filled or ended)';
        this.log.info(`${tag} ended: ${status}`);
        this.log.action({ type: 'ended', id, side: s.side, level: s.level, status });
        this.state.recent[id] = { side: s.side, level: s.level, endedAt: now };
        delete this.state.orders[id];
        return;
      }
      if (s.proven) {
        // Not found: it stays proven and in the rotation (back to it if the node had found it live).
        s.proven.liveAt = null;
        return;
      }
      // Paid in full: the payment is not bound to the ask (another ask's payout serves too, R5T-1),
      // so the ask is not forgotten. It is kept as proven: its level and cap share are free, it is
      // still looked up and cancelled once found, and it ends on the node's word, 2 h past its
      // expiry, or by the operator.
      const proof = r.status === 'gone' && isAsk(s) ? this.paidInFull(id) : null;
      if (proof) {
        const n = Object.values(this.state.orders).filter((x) => x.proven).length;
        if (n < PROVEN_MAX) {
          // Each output proves one fill once, saved with the state file (R4T-2).
          for (const k of proof) this.state.payments[k].usedBy = id;
          s.proven = { at: now, outputs: proof, lookedAt: now, liveAt: null };
          s.unconfirmed = false;
          delete s.provenFull;
          this.log.info(`${tag}: paid in full (its fills are paid in outputs the node showed): kept as proven, its level is free; still looked up on the node until ${PROVEN_KEEP_AFTER_EXPIRY_MS / 3_600_000} h past its expiry`);
          this.log.action({ type: 'proven', id, side: s.side, level: s.level, outputs: proof });
          return;
        }
        if (!s.provenFull) this.alert('warn', 'proven-full', `Order ${tag} is paid in full, but ${n} proven asks are kept already (the most): its level is not freed until one of them ends (found and cancelled, ${PROVEN_KEEP_AFTER_EXPIRY_MS / 3_600_000} h past its expiry, or mm-bot drop).`);
        s.provenFull = true;
      }
      if (!s.unconfirmed) {
        const what = r.status === 'foreign' ? 'names another maker' : !r.verified ? 'is not found under a template not yet proven on the node'
          : `is not found at any state the bot can follow (a fill by an amount the search does not reach, or a full fill)${o ? ', though the indexer lists it' : ''}${sent ? ', and no cancel the bot sent shows on the node' : ''}`;
        this.alert('error', 'node-unconfirmed', `Order ${tag} ${what}: it is kept as unresolved (it counts toward the capital cap, its level is not quoted again, and pulls and STOP wait for it); each cycle the bot looks it up on the node with every amount it knows and cancels it once found. Check it by hand (kob recover --from ${this.p.check} --amount-left ${id}=<amount left>); once it has ended, stop the bot and drop it (mm-bot drop --config FILE --id ${id}), which asks the node first.`);
      }
      s.unconfirmed = true;
      // Past its own expiry, conforming matchers no longer fill it: its level is quoted again,
      // while its tokens still count toward the cap until proof or the operator.
      if (isAsk(s) && !s.parked && s.expiresAt != null && now >= s.expiresAt) {
        s.parked = true;
        this.alert('error', 'unresolved-expired', `Order ${tag} is still unresolved past its expiry: its level is free, but its ${fmtAmount((s.amountLeft ?? 0) / this.scale, 6)} tokens still count toward the ask cap (the ladder may stay a level short), STOP no longer waits for it, and the bot keeps looking for it. Non-conforming takers can still fill it until a refund ends it; check it by hand and, once it has ended, drop it with the bot stopped (mm-bot drop --config FILE --id ${id}): the level comes back.`);
      }
      return;
    }
    this.log.info(`${tag}: the node could not be asked (${r.why}): kept until it shows the order ended`);
    if (!this.tplOf(s).hash && !s.unconfirmed) {
      this.alert('error', 'node-unconfirmed', `Order ${tag} cannot be looked up on the node (no order template known yet: the indexer has listed none of the bot's orders): it is kept (it counts toward the capital cap and its level is not quoted again) until the node shows it ended; check the indexer.`);
      s.unconfirmed = true;
    }
  }

  /** The bot's orders from the state file, in planActions' shape. */
  ordersNow() {
    return Object.entries(this.state.orders).map(([id, s]) => ({
      // A parked ask (past its expiry, unresolved) gives its level back but stays counted; a proven
      // one (paid, R5T-1) gives both back until the node finds it live.
      // A cancel-replaced order (R6T-2) is kept until the node shows the replace; its replacement holds the level.
      id, side: s.side, level: s.parked || s.proven || s.replacedBy ? 1000 + s.level : s.level,
      replaced: !!(s.replacedBy && this.state.orders[s.replacedBy]), price: s.price, quotedFair: s.quotedFair, amountLeft: s.amountLeft ?? s.amount, value: s.value ?? 0,
      proven: !!s.proven && !s.proven.liveAt,
      // An unresolved order (R3T-3) holds its level and is never amended; a pull still cancels it.
      pending: !!s.pending || !!s.unconfirmed || (!!s.proven && !s.proven.liveAt),
      pendingKind: s.pending?.kind ?? (s.unconfirmed ? 'unresolved' : s.proven && !s.proven.liveAt ? 'proven' : null), expiresAt: s.expiresAt ?? null, expired: !!s.expired,
    }));
  }

  /** Logs the fills of the bot's orders that the indexer reports and the bot has not logged yet. */
  async recordFills() {
    const lastId = this.state.lastFillId;
    if (lastId == null) {
      // The first run: start from the newest fill, do not replay history.
      this.state.lastFillId = (await this.idx.fills(this.cfg.token.covenantId, 1))[0]?.id ?? 0;
      return;
    }
    // Every fill since the last read, up to 1,000 (R4T-5): one missed would leave its ask unproven.
    const { items, complete } = await this.idx.fillsSince(this.cfg.token.covenantId, lastId);
    if (!complete) this.alert('warn', 'fills-missed', `More than ${items.length} fills of the token since the last read: older ones are not recorded, so an ask they filled in full may stay unresolved (check it with mm-bot unresolved).`);
    const fresh = items.filter((f) => f.id > lastId).sort((a, b) => a.id - b.id);
    for (const f of fresh) {
      const mine = this.state.orders[f.covenant_id] ?? this.state.recent[f.covenant_id];
      if (!mine) continue;
      const amount = Number(f.amount ?? 0);
      // What the indexer says was filled: a hint for kob recover, which the node checks (R3T-3).
      if (this.state.orders[f.covenant_id] && amount > 0) {
        mine.filledSeen = (mine.filledSeen ?? 0) + amount;
        // Kept to prove a full fill against the payments the node showed (paidInFull).
        if (/^[0-9a-f]{64}$/.test(f.txid ?? '')) mine.fills = [...(mine.fills ?? []), { id: f.id, txid: f.txid, amount, at: Date.now() }].slice(-FILLS_KEEP);
      }
      const price = f.price == null ? null : Number(f.price);
      const tokens = amount / this.scale;
      const kas = price == null ? null : (amount * price) / this.scale / 1e8;
      this.log.info(`FILL ${mine.side === 'sell' ? 'sold' : 'bought'} ${fmtAmount(tokens, 6)} ${this.cfg.token.label ?? 'tokens'} at ${price == null ? '?' : fmtAmount(price / 1e8, 8)} KAS (${kas == null ? '?' : fmtAmount(kas, 4)} KAS), order ${short(f.covenant_id)} L${mine.level}, tx ${f.txid}`);
      this.log.fill({ id: f.covenant_id, side: mine.side, level: mine.level, amount: String(amount), price: price == null ? null : String(price), tokens, kas, txid: f.txid, daa: f.daa, eventId: f.id, payout: f.payout ?? null });
    }
    if (items.length) this.state.lastFillId = Math.max(lastId, items[0].id);
  }

  /** Records transaction ids the bot sent: never taken for a payment to it. */
  ownTx(...txids) {
    const ids = txids.flatMap((t) => String(t ?? '').split(',')).filter((t) => /^[0-9a-f]{64}$/.test(t));
    if (!ids.length) return;
    // Trimmed to OWN_TX_KEEP only by a node read that flagged their outputs as own (R5T-1).
    this.state.ownTx = [...(this.state.ownTx ?? []), ...ids].slice(-(this.nodeOutputs ? OWN_TX_MAX : OWN_TX_KEEP));
    this.sentSince = true;
  }

  /**
   * Records what the node shows paid to the bot's address, by output, so that a payment the bot
   * spends later still counts (R3T-3). One getUtxosByAddresses of its own address, at the start of
   * a cycle, before each placement or amend, and after a cycle that sent. An output keeps the time
   * the node first showed it (`at`) for as long as the node still shows it, so it never comes back
   * as new (R4T-2). An order whose own outpoint has no read after it yet (`ownAt` null) takes
   * this read's time: what this read first showed may predate it. A failure is logged; nothing is
   * proven then. Returns whether the node answered.
   */
  async seePayments(now) {
    this.sentSince = false;
    if (!this.nodeOutputs || !this.maker || !this.address) return false;
    let outs;
    try {
      outs = await this.nodeOutputs(this.address, this.maker);
    } catch (e) {
      this.log.info(`payments to the bot not read from the node: ${e.message}`);
      return false;
    }
    const own = new Set(this.state.ownTx ?? []);
    const pay = this.state.payments;
    const shown = new Set();
    for (const u of outs) {
      if (!Number.isSafeInteger(u.index)) continue;
      const k = `${u.txid}:${u.index}`;
      shown.add(k);
      pay[k] ??= { txid: u.txid, sompi: u.amount, at: now };
      // An output of the bot's own transaction (its change) is flagged own while its txid is in
      // ownTx, and keeps the flag: it never counts as a payment later (R5T-1).
      if (own.has(u.txid)) pay[k].own = true;
      delete pay[k].goneAt;
    }
    // One not shown is kept for a week after the node last showed it, so that one read that omits
    // it does not make it new again (R5T-I1); the records of no use yet, at most PAYMENTS_KEEP.
    for (const [k, p] of Object.entries(pay)) if (!shown.has(k)) p.goneAt ??= now;
    const gone = Object.entries(pay).filter(([k, p]) => !shown.has(k) && now - p.goneAt < PAYMENT_KEEP_MS);
    const plain = gone.filter(([, p]) => !p.own && !p.usedBy).sort((a, b) => b[1].goneAt - a[1].goneAt).slice(0, PAYMENTS_KEEP);
    this.state.payments = Object.fromEntries([...Object.entries(pay).filter(([k]) => shown.has(k)), ...gone.filter(([, p]) => p.own || p.usedBy), ...plain]);
    // Their outputs are flagged now: the list of own txids can roll.
    this.state.ownTx = (this.state.ownTx ?? []).slice(-OWN_TX_KEEP);
    for (const o of Object.values(this.state.orders)) if (o.ownAt == null) o.ownAt = now;
    return true;
  }

  /**
   * The payment outputs that prove ask `id` filled in full, or null. The fills the indexer
   * reported must add up to what the ask's last own outpoint held (`ownAmount`), each paid in its
   * transaction by one output to the bot (not in one of the bot's own transactions) of at least
   * what the covenant pays the maker, ceil(n x (price - tip) / scale). The output must be one the
   * node first showed after that outpoint was sent (`ownAt`), and one that proved no other fill
   * (R4T-2): a lying indexer then needs a real payment to the bot, made after the placement or
   * amend, of the ask's whole proceeds, to make the bot forget it.
   */
  paidInFull(id) {
    const s = this.state.orders[id];
    const base = s?.ownAmount ?? s?.amountLeft ?? s?.amount;
    if (!s?.fills?.length || !(base > 0) || s.ownAt == null) return null;
    const need = (o, n) => {
      const st = o.own?.state?.state ?? {};
      const p = BigInt(st.price ?? Math.round(o.price ?? 0));
      const tip = BigInt(st.tip ?? 0);
      const sc = BigInt(st.scale ?? this.scale);
      return p > tip ? (BigInt(n) * (p - tip) + sc - 1n) / sc : null;
    };
    const own = new Set(this.state.ownTx ?? []);
    const picked = [];
    let proven = 0;
    for (const f of s.fills) {
      const q = need(s, f.amount);
      if (q == null || own.has(f.txid)) continue;
      // The smallest free output of the fill's transaction that covers it.
      const k = Object.entries(this.state.payments)
        .filter(([key, p]) => p.txid === f.txid && (!p.usedBy || p.usedBy === id) && !p.own && !picked.includes(key) && p.at > s.ownAt && BigInt(p.sompi) >= q)
        .sort((a, b) => a[1].sompi - b[1].sompi)[0]?.[0];
      if (!k) continue;
      picked.push(k);
      proven += f.amount;
    }
    return proven >= base ? picked : null;
  }

  /**
   * The orders kept with no proof that they ended (R3T-3): unresolved, parked past their expiry,
   * or proven paid but not seen ended on the node (R5T-1).
   */
  unresolvedOrders() {
    return Object.entries(this.state.orders).filter(([, s]) => s.unconfirmed || s.parked || s.proven).map(([id, s]) => ({
      id, side: s.side, level: s.level, price: s.price, amount: s.amount ?? null, amountLeft: s.amountLeft ?? null, filledSeen: s.filledSeen ?? 0,
      parked: !!s.parked, proven: !!s.proven, placedAt: s.placedAt ?? null, expiresAt: s.expiresAt ?? null, hints: isAsk(s) ? this.hintsOf(id, null, { pay: false }) : [],
    }));
  }

  /**
   * The operator's drop of one unresolved, parked or proven order, after checking it (R4T-5). The
   * node is asked first, from the bot's record and every amount known (plus `amountLeft`, the
   * operator's): an order it shows live, or that it cannot be asked about, is kept. Without
   * `force` an order is kept too when the lookup proves nothing (its template is not proven on the
   * node) or the indexer reported fills of it since its last own outpoint (its amount left may be
   * one the search did not reach) (R5T-3). A dropped order's record is appended to
   * dropped-orders.json, which kob recover reads. Returns { ok, node, message }.
   */
  async operatorDrop(id, { amountLeft = null, force = false } = {}) {
    const s = this.state.orders[id];
    if (!s) return { ok: false, node: null, message: `${id} is not in state.json` };
    if (!s.unconfirmed && !s.parked && !s.proven) return { ok: false, node: null, message: `${short(id)} is not unresolved: the bot tracks it and ends it by itself` };
    const bal = await this.kob.balances(this.cfg.token.covenantId);
    this.maker = bal.maker;
    const v = await this.idx.order(id).catch(() => null);
    if (v?.amountLeft > 0) s.idxAmount = v.amountLeft;
    const hints = isAsk(s) ? [...new Set([...this.hintsOf(id), ...(Number.isSafeInteger(amountLeft) && amountLeft > 0 ? [amountLeft] : [])])] : [];
    const r = (await this.nodeCheck([id], hints.length ? { [id]: hints } : {})).get(id);
    if (r.status === 'live') return { ok: false, node: r, message: `the node holds ${short(id)} live at ${r.txid} (amount left ${r.amountLeft ?? '?'}): not dropped; cancel it (kob cancel ${id})` };
    if (r.status === 'unknown') return { ok: false, node: r, message: `the node could not be asked about ${short(id)} (${r.why}): not dropped` };
    const what = r.status === 'foreign' ? 'names another maker' : `is not found on the node at the amounts ${hints.join(', ') || '(a bid: its script)'}${isAsk(s) ? ' (the search is not exhaustive)' : ''}`;
    const fills = (s.fills ?? []).filter((f) => f.at == null || s.ownAt == null || f.at > s.ownAt);
    const doubt = r.status !== 'gone' ? null
      : !r.verified ? `it was looked up under an order template not yet proven on the node (no order of its kind found live under it), so not finding it proves nothing`
        : isAsk(s) && fills.length ? `the indexer reported ${fills.length} fill(s) of it since its last placement or amend (${fills.reduce((a, f) => a + f.amount, 0)} base units in all): it may be live at an amount left the search did not reach`
          : null;
    const warn = 'A dropped order that is still live stays fillable, and the bot no longer cancels it (not by a pull, IDLE or STOP).';
    if (doubt && !force) {
      return { ok: false, node: r, message: `${short(id)} ${what}, but ${doubt}: not dropped. Check it first (kob recover --from ${this.p.unresolved} --amount-left ${id}=<amount left>, after mm-bot unresolved), then drop it with --force. ${warn}` };
    }
    const at = new Date().toISOString();
    const why = `the operator: it ${what}${doubt ? `; forced past: ${doubt}` : ''}`;
    this.recordDropped(id, { at, why, node: r.status, hints, forced: !!(doubt && force) });
    delete this.state.orders[id];
    this.state.recent[id] = { side: s.side, level: s.level, endedAt: Date.now() };
    this.log.info(`${short(id)} dropped by the operator: it ${what}${doubt ? ` (forced: ${doubt})` : ''}`);
    this.log.action({ type: 'operator-drop', id, side: s.side, level: s.level, node: r.status, hints, forced: !!(doubt && force), record: this.p.dropped });
    this.save();
    return { ok: true, node: r, message: `${id} ${what}: dropped from state.json. ${warn} Its record is kept in ${this.p.dropped}; to look for it later: kob recover --from ${this.p.dropped} --network ${this.cfg.network} --amount-left ${id}=<amount left> (add --cancel to cancel it once found).` };
  }

  /** Appends a dropped order's record to dropped-orders.json, in the shape kob recover reads (R5T-3). */
  recordDropped(id, info) {
    const s = this.state.orders[id];
    let doc = { items: [] };
    if (existsSync(this.p.dropped)) {
      try {
        doc = JSON.parse(readFileSync(this.p.dropped, 'utf8'));
      } catch (e) {
        throw new Error(`${this.p.dropped} does not read (${e.message}): fix it before dropping another order; nothing dropped`);
      }
    }
    const view = this.recordOf(id) ?? { covenant_id: id, template_hash: this.tplOf(s).hash, status: 'open', state_known: false };
    doc.items = [...(doc.items ?? []), { ...view, dropped: { ...info, side: s.side, level: s.level, price: s.price, amount: s.amount ?? null, amountLeft: s.amountLeft ?? null,
      filledSeen: s.filledSeen ?? 0, lastTxid: s.own?.txid ?? null, prevState: s.own?.prev ?? null, placedAt: s.placedAt ?? null, expiresAt: s.expiresAt ?? null, proven: s.proven ?? null, fills: s.fills ?? [] } }];
    if (!this.dryRun) writeFileSync(this.p.dropped, `${JSON.stringify(doc, null, 1)}\n`);
  }

  /** The target ladder and the cycle's actions. */
  plan(snap, bal, orders) {
    const c = this.cfg;
    const q = sizedQuotes(c.capitalUsd, c.quotes, snap.tokenUsd.value, snap.kasUsd);
    const mine = holdings(orders);
    const budgets = sideBudgets({ kasFree: bal.kasFree, tokenFree: bal.token.free }, mine, q, this.scale);
    const tokenValueKas = ((bal.token.free + mine.askTokens) / this.scale) * snap.fair;
    const shift = skewShift(tokenValueKas, kasOf(bal.kasFree + mine.bidKas), c.quotes.skew);
    const targets = buildLadder(snap.fair, budgets, q, this.scale, shift);
    // New placements fit inside the side caps less what is on chain already: every tracked order,
    // including one the indexer does not show, and every placement of unknown outcome (R2-T1).
    const on = committed(orders, this.state.blind ?? [], this.scale);
    const room = { kas: Math.floor(q.maxBidKas * 1e8) - on.bidKas, tokens: Math.floor(q.maxAskTokens * this.scale) - on.askTokens };
    const free = { kas: Math.max(0, Math.min(room.kas, bal.kasFree - Math.floor(c.quotes.kasReserve * 1e8))), tokens: Math.max(0, Math.min(room.tokens, bal.token.free)) };
    let actions = planActions(targets, orders, { fair: snap.fair, scale: this.scale, free, q, now: Date.now(), life: this.life() });
    // The token program takes at most 3 token inputs per transaction: merge the free token UTXOs
    // before placing an ask.
    const asksToPlace = actions.some((a) => a.kind === 'place' && a.side === 'sell');
    if (asksToPlace && bal.token.freeUtxos > c.quotes.mergeAboveUtxos) {
      actions = [...actions.filter((a) => !(a.kind === 'place' && a.side === 'sell')), { kind: 'merge', reason: `${bal.token.freeUtxos} free token UTXOs` }];
    }
    return {
      q, budgets, shift, targets, mine, actions,
      sides: { bids: sideStatus(targets.bids, budgets.bidKas), asks: sideStatus(targets.asks, budgets.askTokens) },
    };
  }

  /** The quotes' life: placed and amended with an expiry lifeMs ahead, renewed after renewAfterMs. */
  life() {
    return { lifeMs: this.cfg.quotes.lifeMin * 60_000, renewAfterMs: this.cfg.quotes.renewAfterMin * 60_000 };
  }

  /** Runs the actions in order, within the fee gate and the per-cycle limit. */
  async execute(actions) {
    const c = this.cfg;
    let n = 0;
    let lastPlace = 0;
    for (const a of actions) {
      if (n >= c.safety.maxActionsPerCycle) {
        this.log.info(`action limit (${c.safety.maxActionsPerCycle}) reached; ${actions.length - n} action(s) wait for the next cycle`);
        break;
      }
      const isCancel = a.kind === 'cancel';
      if (!isCancel && this.stopRequested()) break;
      if (!isCancel && !feeGate(feesToday(this.state), c.safety).canTrade) {
        this.log.info(`fee budget for amends and placements spent (${kasOf(feesToday(this.state)).toFixed(4)} KAS today); ${a.kind} skipped`);
        continue;
      }
      const own = this.crossesOwn(a);
      if (own != null) {
        this.log.info(`${a.kind} ${a.side} L${a.level} at ${a.price} sompi skipped: it would meet the bot's own ${a.side === 'buy' ? 'ask' : 'bid'} at ${own} (no self-trade)`);
        continue;
      }
      if ((a.kind === 'place' || a.kind === 'merge') && lastPlace && Date.now() - lastPlace < c.safety.placeGapSec * 1000) await wait(c.safety.placeGapSec * 1000 - (Date.now() - lastPlace));
      n++;
      const r = await this.run1(a);
      if (a.kind === 'place' || a.kind === 'merge') lastPlace = Date.now();
      if (a.kind === 'merge' && r.ok) break; // the asks are placed in the next cycle, from the merged UTXO
    }
  }

  /**
   * The price of the bot's own order on the other side that a placement or an amend would meet
   * (a buy at or above its lowest ask, a sell at or below its highest bid), else null (T-7).
   */
  crossesOwn(a) {
    if (a.kind !== 'place' && a.kind !== 'amend') return null;
    // A proven ask (R5T-1) counts until its expiry, or while the node holds it live: it may be live.
    const now = Date.now();
    const open = (s) => !s.proven || s.proven.liveAt || (s.expiresAt ?? s.proven.at + this.life().lifeMs) > now;
    const other = Object.entries(this.state.orders).filter(([id, s]) => id !== a.id && s.side !== a.side && s.price > 0 && open(s)).map(([, s]) => s.price);
    if (!other.length) return null;
    const edge = a.side === 'buy' ? Math.min(...other) : Math.max(...other);
    return (a.side === 'buy' ? a.price >= edge : a.price <= edge) ? edge : null;
  }

  /**
   * While placements are refused (R2-T3): a dry run of the first placement due. When it passes, or
   * none is due, the count clears and the bot resumes by itself.
   */
  async probePlacement(snap, bal, orders) {
    const a = this.plan(snap, bal, orders).actions.find((x) => x.kind === 'place');
    const r = a ? await this.kob.probePlace(this.cfg.token.covenantId, a.side, a.amount, a.price, gtdArg(Date.now() + this.life().lifeMs)) : { ok: true };
    if (r.ok) {
      this.log.info('a planned placement passes again: placements resume after the cool-down');
      this.state.placeFails = 0;
      this.placeError = null;
    } else this.placeError = r.error;
  }

  /** One action through the CLI, with its bookkeeping. */
  async run1(a) {
    const c = this.cfg;
    const tok = c.token.covenantId;
    const desc = a.kind === 'place' ? `place ${a.side} L${a.level} ${fmtAmount(a.amount / this.scale, 6)} @ ${fmtAmount(a.price / 1e8, 8)} KAS`
      : a.kind === 'amend' ? `${a.renewOnly ? 'renew' : 'amend'} ${short(a.id)} ${a.side} L${a.level} ${fmtAmount(a.from / 1e8, 8)} -> ${fmtAmount(a.price / 1e8, 8)} KAS`
        : a.kind === 'cancel' ? `cancel ${short(a.id)} ${a.side} L${a.level}` : `merge ${c.token.label ?? 'token'}`;
    // Every placement and amend sets the quote's expiry lifeMin ahead (to the second, at or before it on chain).
    const expiresAt = Math.floor((Date.now() + this.life().lifeMs) / 1000) * 1000;
    const gtd = gtdArg(expiresAt);
    // The payments shown before a new own outpoint never prove its fills (R4T-2): read them first.
    // Without an answer the next read that succeeds stands for the time (ownAt null).
    const sends = (a.kind === 'place' || a.kind === 'amend') && !this.dryRun;
    const ownAt = sends && (await this.seePayments(Date.now())) ? Date.now() : null;
    // A merge spends the wallet's KAS too: what landed since the last read is recorded first (R6T-1).
    if (a.kind === 'merge' && !this.dryRun) await this.seePayments(Date.now());
    const r = a.kind === 'place' ? await this.kob.place(tok, a.side, a.amount, a.price, gtd)
      : a.kind === 'amend' ? await this.kob.amend(a.id, a.price, gtd)
        : a.kind === 'cancel' ? await this.cancelOrder(a.id) : await this.kob.merge(tok);
    const rec = { type: a.kind, ...a, gtd: a.kind === 'place' || a.kind === 'amend' ? gtd : null, status: r.ok ? (r.dryRun ? 'planned' : 'submitted') : 'failed', txid: r.txid ?? null, fee: r.fee ?? null, newIds: r.newIds ?? [], error: r.error ?? null };
    this.log.action(rec);
    if (!r.ok) {
      this.log.info(`${desc} (${a.reason}): FAILED ${r.error}`);
      // The order's state is unknown now: leave it alone until the next cycle has re-read it.
      if (a.id && this.state.orders[a.id]) this.state.orders[a.id].pending = { kind: 'reread', since: Date.now() };
      if (a.kind === 'place' && !this.dryRun) this.placeFailed(a, r);
      // A send that may have gone out counts its planned fee toward the daily cap (L25).
      if (r.sentFee && !this.dryRun) {
        addFee(this.state, `${a.kind}-unknown`, r.sentFee);
        this.ownTx(r.doc?.result?.txid ?? r.doc?.transaction?.txid);
      }
      this.save();
      return r;
    }
    this.log.info(`${desc} (${a.reason}): ${r.dryRun ? 'planned (dry run)' : 'submitted'}${r.txid ? ` tx ${r.txid}` : ''}, fee ${fmtAmount(kasOf(r.fee), 8)} KAS`);
    if (r.dryRun) return r;
    // A renewal that moves no price is the cost of the expiry: its own line in the fee ledger.
    addFee(this.state, a.renewOnly ? 'renew' : a.kind, r.fee);
    this.ownTx(...(r.txids ?? [r.txid]));
    const now = Date.now();
    if (a.kind === 'place') {
      const id = r.newIds[0];
      // The order is tracked from the CLI's own result: its state and outpoint (R2-T1).
      if (id) this.state.orders[id] = { ...this.placed(a, r.doc, r.txid, now, expiresAt), ownAt, pending: { kind: 'place', since: now, txid: r.txid } };
      this.state.placeFails = 0;
      this.placeError = null;
    } else if (a.kind === 'amend') {
      const s = this.state.orders[a.id];
      const inPlace = r.doc?.plan?.inPlace !== false && !r.newIds.length;
      if (inPlace) {
        // Until the amend is seen, the order may still sit under its earlier state (own.prev).
        const rep = r.doc?.plan?.replacement;
        s.own = { ...(s.own ?? { tpl: null }), prev: s.own?.state ?? null, state: rep?.kind ? rep : s.own?.state ?? null, txid: r.txid };
        if (/^\d+$/.test(String(r.doc?.plan?.value ?? ''))) s.value = Number(r.doc.plan.value);
        // What the new outpoint holds: the fills before it are in it already.
        const left = rep?.state?.amountLeft;
        Object.assign(s, { price: a.price, quotedFair: a.quotedFair, expiresAt, expired: false, ownAt, ownAmount: /^\d+$/.test(String(left ?? '')) ? Number(left) : s.amountLeft ?? s.amount,
          pending: { kind: 'amend', since: now, txid: r.txid } });
      }
      else {
        // A cancel-replace: the order has a new id. The old record stays, as cancelled, until the
        // node shows the replace (an output of it, or the replacement live): a fill that lands
        // first leaves the old order live (R6T-2).
        const id = r.newIds[0];
        if (id) this.state.orders[id] = { ...this.placed({ ...s, ...a, amount: s.amountLeft ?? s.amount }, r.doc, r.txid, now, expiresAt), ownAt, pending: { kind: 'place', since: now, txid: r.txid } };
        s.replacedBy = id ?? null;
        this.noteCancel(s, r.txid, now);
        this.log.info(`${short(a.id)} was cancel-replaced (not in place): new id ${id}; the old record is kept until the node shows the replace`);
      }
    } else if (a.kind === 'cancel') {
      const s = this.state.orders[a.id];
      if (s) this.noteCancel(s, r.txid, now);
    }
    this.save();
    return r;
  }

  /** The state-file record of an order the bot placed, from the CLI's plan of it (R2-T1). */
  placed(a, doc, txid, now, expiresAt) {
    const st = doc?.plan?.states?.[0];
    const value = Number(doc?.plan?.request?.value ?? 0) || (a.side === 'buy' ? bidCost(a.amount, a.price, this.scale) : 0);
    return { side: a.side, level: a.level, price: a.price, amount: a.amount, ownAmount: a.amount, ownAt: null, quotedFair: a.quotedFair, placedAt: now, expiresAt, amountLeft: a.amount, value,
      own: st?.kind ? { state: st, prev: null, txid: txid ?? null, tpl: null } : null };
  }

  /**
   * A placement that failed. One that may have gone out is tracked under the covenant id its plan
   * names, its outcome unknown until the node says (or, with no id known, counted toward the caps for
   * a day). A refusal counts toward IDLE/placement (R2-T3).
   */
  placeFailed(a, r) {
    const now = Date.now();
    if (r.sentFee) {
      const id = r.doc?.new_covenant_ids?.[0];
      const txid = r.doc?.result?.txid ?? r.doc?.transaction?.txid ?? null;
      if (id && /^[0-9a-f]{64}$/.test(id)) {
        this.state.orders[id] = { ...this.placed(a, r.doc, txid, now, null), fate: 'unknown', pending: { kind: 'place', since: now, txid } };
      } else this.state.blind.push({ side: a.side, cost: a.side === 'buy' ? bidCost(a.amount, a.price, this.scale) : 0, tokens: a.side === 'sell' ? a.amount : 0, at: now });
      this.alert('warn', 'place-unknown', `A placement (${a.side} L${a.level}) may have gone out (${r.error}); it counts toward the caps ${id ? `as order ${short(id)} until the node says` : 'for a day (no covenant id known)'}.`);
      return;
    }
    this.state.placeFails = (this.state.placeFails ?? 0) + 1;
    this.placeError = r.error;
  }

  checkFunds(snap, bal, orders, plan) {
    const now = Date.now();
    const c = this.cfg;
    const q = plan?.q ?? sizedQuotes(c.capitalUsd, c.quotes, snap.tokenUsd.value, snap.kasUsd);
    const mine = plan?.mine ?? holdings(orders);
    // While idle, judge each side by the ladder it would build.
    const targets = plan?.targets ?? buildLadder(snap.fair, sideBudgets({ kasFree: bal.kasFree, tokenFree: bal.token.free }, mine, q, this.scale), q, this.scale, 0);
    const checks = fundChecks({
      fair: snap.fair, q, capitalUsd: c.capitalUsd, warnBelowPct: c.alerts.warnBelowPct, rates: { tokenUsd: snap.tokenUsd.value, kasUsd: snap.kasUsd },
      address: this.address, token: { id: c.token.covenantId, label: c.token.label },
      kasHeld: kasOf(bal.kasFree + mine.bidKas), kasFree: kasOf(bal.kasFree), tokenHeld: (bal.token.free + mine.askTokens) / this.scale,
      bidsQuoting: targets.bids.length > 0, asksQuoting: targets.asks.length > 0,
    });
    this.funds = checks;
    for (const a of alertsToSend(checks, this.state.alerts, now, c.alerts.remindMin)) {
      this.alert(a.level, `funds-${a.key}`, a.message, { asset: a.asset, unit: a.unit, held: a.held, needed: a.needed, shortfall: a.shortfall });
    }
  }

  /** One alert to the logs, alerts.jsonl and the optional webhook. */
  alert(level, reason, message, extra = {}, mode = this.state.mode) {
    const rec = { level, state: mode.mode, reason, message, asset: extra.asset ?? null, unit: extra.unit ?? null, held: extra.held ?? null,
      needed: extra.needed ?? null, shortfall: extra.shortfall ?? null, address: this.address ?? null, network: this.cfg.network };
    this.log.info(`${level.toUpperCase()}: ${message}`);
    this.log.action({ type: 'alert', ...rec });
    if (this.dryRun) return;
    this.log.alert(rec);
    if (this.cfg.alerts.webhookUrl) this.webhook(rec);
  }

  /**
   * Retired tokens: cancel the bot's orders and merge the returned UTXOs. Never place or amend. At
   * most FOREIGN_PER_CYCLE cancels a cycle in all, and none starts once RETIRED_BUDGET_MS have
   * gone in this call (R4T-4), so that the next reprice of the live quotes waits for them no longer
   * than that plus one call.
   */
  async retire(bal, stopping) {
    const now = Date.now();
    let cancels = 0;
    for (const id of this.cfg.retiredTokens) {
      const r = (this.state.retired[id] ??= { done: false, cancels: {} });
      r.failed ??= {};
      // An order whose cancel failed FOREIGN_TRIES times (one anyone made under the key, with no
      // custody) is given up, so that it neither repeats every cycle nor holds the clean-up (R3T-2).
      const listed = await this.idx.liveOrders(this.maker, id);
      // Only the orders still listed keep a failure count.
      r.failed = Object.fromEntries(Object.entries(r.failed).filter(([oid]) => listed.some((o) => o.id === oid)));
      const given = listed.filter((o) => (r.failed[o.id] ?? 0) >= FOREIGN_TRIES);
      const live = listed.filter((o) => !given.includes(o)).map((o) => ({ ...o, cancelSentAt: r.cancels[o.id] ?? null }));
      const row = bal.tokens?.[id] ?? { free: 0, freeUtxos: 0, decimals: null };
      const { actions, done } = retiredPlan(live, row.freeUtxos, now, this.cfg.safety.orderVisibleTimeoutSec * 1000);
      if (done) {
        if (!r.done) this.alert('info', 'retired', retiredMessage(id, row.free, row.decimals), { asset: id });
        r.done = true;
        r.cancels = {};
        continue;
      }
      r.done = false;
      for (const a of actions) {
        if (a.kind === 'merge' && (stopping || now - (r.mergeSentAt ?? 0) < this.cfg.safety.orderVisibleTimeoutSec * 1000)) continue;
        if (a.kind === 'cancel' && (cancels >= FOREIGN_PER_CYCLE || Date.now() - now > RETIRED_BUDGET_MS)) continue;
        if (a.kind === 'cancel') cancels++;
        const res = a.kind === 'cancel' ? await this.kob.cancel(a.id) : await this.kob.merge(id);
        const status = res.ok ? (res.dryRun ? 'planned' : 'submitted') : 'failed';
        this.log.action({ type: `retired-${a.kind}`, token: id, id: a.id ?? null, status, txid: res.txid ?? null, fee: res.fee ?? null, error: res.error ?? null });
        this.log.info(`retired ${short(id)}: ${a.kind}${a.id ? ` ${short(a.id)}` : ''}: ${res.ok ? `${status} tx ${res.txid ?? '-'}, fee ${fmtAmount(kasOf(res.fee ?? 0), 8)} KAS` : `FAILED ${res.error}`}`);
        if (!res.ok && a.kind === 'cancel' && (r.failed[a.id] = (r.failed[a.id] ?? 0) + 1) === FOREIGN_TRIES) {
          this.alert('warn', 'retired-cancel-failed', `retired ${short(id)}: the cancel of ${short(a.id)} failed ${FOREIGN_TRIES} times (${res.error}); it is not tried again. Check it by hand (kob recover, kob cancel).`, { asset: id });
        }
        if (res.ok && !res.dryRun) {
          this.ownTx(...(res.txids ?? [res.txid]));
          addFee(this.state, `retired-${a.kind}`, res.fee);
          if (a.kind === 'cancel') r.cancels[a.id] = now;
          else r.mergeSentAt = now;
        }
      }
    }
  }

  /**
   * The optional webhook: a POST of the alert as JSON. A failure is logged and otherwise ignored.
   */
  webhook(rec) {
    const body = { ...rec, ts: new Date().toISOString() };
    fetch(this.cfg.alerts.webhookUrl, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body), signal: AbortSignal.timeout(10_000) })
      .then((res) => { if (!res.ok) this.log.info(`webhook: HTTP ${res.status}`); })
      .catch((e) => this.log.info(`webhook failed: ${e.message}`));
  }

  snapshot(snap, bal, orders, after, plan, sides, now) {
    const c = this.cfg;
    const mine = holdings(orders);
    const tokens = (bal.token.free + mine.askTokens) / this.scale;
    const kas = kasOf(bal.kasFree + mine.bidKas);
    const inv = snap.ok ? inventoryUsd(tokens, kas, { tokenUsd: snap.tokenUsd.value, kasUsd: snap.kasUsd }, c.capitalUsd) : null;
    if (inv && plan) this.log.info(`inventory: ${inv.text}; ${plan.targets.bids.length} bid / ${plan.targets.asks.length} ask level(s) targeted, ${orders.length} live`);
    return {
      at: now,
      mode: this.state.mode,
      reference: { ok: snap.ok, reason: snap.reason ?? null, kasUsd: snap.kasUsd ?? null, fair: snap.fair ?? null, tokenUsd: snap.tokenUsd ?? null, usdtUsd: snap.usdtUsd ?? null, used: snap.used, dropped: snap.dropped, venues: snap.venues, jump: snap.jump ?? 0 },
      address: this.address,
      wallet: { kasFree: bal.kasFree, kasTotal: bal.kasTotal, tokenFree: bal.token.free, tokenUtxos: bal.token.freeUtxos },
      inventory: { tokens, kas, ...(inv ?? {}) },
      sides,
      quotes: after.map((o) => ({ id: o.id, side: o.side, level: o.level, price: o.price, amountLeft: o.amountLeft, value: o.value, quotedFair: o.quotedFair, pending: o.pendingKind,
        expiresAt: o.expiresAt == null ? null : new Date(o.expiresAt).toISOString(), expired: o.expired })),
      funds: (this.funds ?? []).map((f) => ({ key: f.key, level: f.level, message: f.message })),
      fees: this.state.fees,
      dryRun: this.dryRun,
    };
  }
}

export { IDLE, QUOTING, STOPPING };
