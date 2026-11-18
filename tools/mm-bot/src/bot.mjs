// The cycle: reference, wallet and orders, reconcile, mode, actions, funds. Only the operator ends
// the loop.

import { existsSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { addressOfXOnly } from './address.mjs';
import { readReference } from './feeds.mjs';
import { alertsToSend, fmtAmount, fundChecks } from './funds.mjs';
import { Indexer, indexerUrl } from './indexer.mjs';
import { sideBudgets, skewShift } from './inventory.mjs';
import { Kob } from './kob.mjs';
import { buildLadder } from './ladder.mjs';
import { backoffMs, feeGate, IDLE, initialMode, nextMode, QUOTING, sideStatus, STOPPING } from './modes.mjs';
import { planActions, pullAll } from './reprice.mjs';
import { retiredMessage, retiredPlan } from './retired.mjs';
import { inventoryUsd, sizedQuotes } from './sizing.mjs';
import { addFee, feesToday, loadState, Logger, paths, saveState } from './store.mjs';

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const kasOf = (sompi) => sompi / 1e8;
const ENDED = new Set(['filled', 'cancelled', 'refunded', 'killed', 'closed']);
const short = (id) => `${id.slice(0, 8)}..${id.slice(-4)}`;
const ERROR_ALERT_AFTER = 3; // consecutive failed cycles before an error alert

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
    case 'fee-cap':
      return { level: 'warn', message: `Quotes pulled: today's fees reached ${(feesSompi / 1e8).toFixed(4)} KAS of the ${c.safety.maxFeesKasPerDay} KAS cap; the bot resumes at 00:00 UTC (raise safety.maxFeesKasPerDay if the spend is expected).` };
    case 'cooldown':
      return prev.mode === 'IDLE' && prev.reason !== 'cooldown' ? { level: 'info', message: `The condition cleared (${prev.reason}); quoting resumes after a ${c.safety.resumeAfterSec} s cool-down.` } : null;
    default:
      return null;
  }
}

/** What the bot's own orders hold: tokens in asks (base units), KAS escrow in bids (sompi). */
const holdings = (orders) => ({
  askTokens: orders.filter((o) => o.side === 'sell').reduce((a, o) => a + o.amountLeft, 0),
  bidKas: orders.filter((o) => o.side === 'buy').reduce((a, o) => a + o.value, 0),
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
    // `once` skips the start-up cool-down.
    if (once && this.state.mode.reason === 'cooldown') this.state.mode = { ...this.state.mode, steadySince: Date.now() - cfg.safety.resumeAfterSec * 1000 };
    this.refMemory = {};
    this.scale = 10 ** cfg.token.decimals;
    this.stopSignal = false;
    this.failures = 0;
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
    this.log.info(`start: ${c.network}, token ${c.token.label ?? ''} ${c.token.covenantId}, indexer ${indexerUrl(c)}, node ${c.node ?? '(CLI default)'}${this.dryRun ? ', DRY RUN (nothing is signed)' : ''}`);
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

    let bal;
    let orders;
    try {
      bal = await this.kob.balances(c.token.covenantId);
      this.maker = bal.maker;
      this.address ??= addressOfXOnly(bal.maker, c.network);
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
    await this.recordFills();

    const gate = feeGate(feesToday(this.state, now), c.safety);
    const prev = this.state.mode;
    const mode = nextMode(prev, { stop, refOk: snap.ok, jump: snap.jump ?? 0, feeBlocked: !gate.canTrade, pegOk: snap.peg?.ok ?? true }, c.safety, now);
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

    let sides = { bids: { quoting: false, reason: 'idle' }, asks: { quoting: false, reason: 'idle' } };
    let plan = null;
    if (mode.pull) {
      const toCancel = orders.filter((o) => o.pendingKind !== 'cancel');
      if (toCancel.length) {
        this.log.info(`pull ${toCancel.length} quote(s): ${mode.mode}${mode.reason ? `/${mode.reason}` : ''}`);
        await this.execute(pullAll(toCancel, `pull: ${mode.reason ?? mode.mode}`));
      }
    } else {
      plan = this.plan(snap, bal, orders);
      sides = plan.sides;
      await this.execute(plan.actions);
    }

    if (c.retiredTokens.length) await this.retire(bal, mode.mode === STOPPING);
    const after = this.ordersNow();
    if (snap.ok && !stop) this.checkFunds(snap, bal, orders, plan);
    this.state.last = this.snapshot(snap, bal, orders, after, plan, sides, now);
    this.save();

    if (mode.mode === STOPPING) {
      const left = (await this.idx.liveOrders(this.maker, c.token.covenantId)).length;
      if (left === 0 || this.dryRun) {
        this.log.info('stopped: no quote left');
        return 'stopped';
      }
      this.log.info(`stopping: ${left} order(s) still listed; waiting for the cancels (a cancel not seen in ${c.safety.orderVisibleTimeoutSec} s is sent again)`);
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
   * The reads failed and the mode says pull (STOP, no reference, a jump, the peg).
   * It cancels the bot's orders from the last saved indexer views with `kob cancel --view`, which
   * checks each on the node and needs no indexer. An order amended, filled or placed since that
   * view is refused as stale and waits for the indexer.
   */
  async blindPull(stop, snap, now, cause) {
    const c = this.cfg;
    const mode = nextMode(this.state.mode, { stop, refOk: snap.ok, jump: snap.jump ?? 0, feeBlocked: false, pegOk: snap.peg?.ok ?? true }, c.safety, now);
    if (!mode.pull) return;
    const timeout = c.safety.orderVisibleTimeoutSec * 1000;
    const due = Object.entries(this.state.orders).filter(([, s]) => s.pending?.kind !== 'cancel' || now - s.pending.since > timeout);
    if (!due.length) return;
    const { changed: _c, pull: _p, ...rec } = mode;
    this.state.mode = rec;
    if (!existsSync(this.p.views)) {
      this.alert('error', 'blind-pull', `Quotes are due to be pulled (${mode.reason ?? mode.mode}) but the wallet or indexer read failed (${cause.message}) and no order view is saved: cancel ${due.length} order(s) by hand.`, {}, mode);
      this.save();
      return;
    }
    this.log.info(`pull ${due.length} quote(s) without the indexer (${cause.message}): kob cancel --view ${this.p.views}`);
    const r = await this.kob.cancelViews(this.p.views);
    this.log.action({ type: 'blind-cancel', reason: mode.reason ?? mode.mode, ids: r.ids, txids: r.txids, fee: r.fee, error: r.error });
    if (!this.dryRun) {
      if (r.fee) addFee(this.state, 'cancel', r.fee);
      for (const id of r.ids) if (this.state.orders[id]) this.state.orders[id].pending = { kind: 'cancel', since: now, txid: null };
    }
    const left = due.length - r.ids.length;
    this.alert(left ? 'error' : 'warn', 'blind-pull', `Quotes pulled without the indexer (${mode.reason ?? mode.mode}): ${r.ids.length} cancel(s) sent through the node${left ? `; ${left} order(s) could not be cancelled from the saved views (${r.error ?? 'stale'}): check them by hand` : ''}.`, {}, mode);
    this.save();
  }

  /**
   * The last indexer views of the bot's live orders, for kob cancel --view (written only when they
   * change).
   */
  saveViews() {
    if (this.dryRun) return;
    const items = this.idx.views[this.cfg.token.covenantId] ?? [];
    const text = `${JSON.stringify({ items }, null, 1)}\n`;
    try {
      if (existsSync(this.p.views) && readFileSync(this.p.views, 'utf8') === text) return;
    } catch { /* rewrite */ }
    writeFileSync(`${this.p.views}.tmp`, text);
    renameSync(`${this.p.views}.tmp`, this.p.views);
  }

  /**
   * Syncs the state file with the indexer and returns the live orders. An unknown order is adopted
   * to be cancelled.
   */
  async reconcile(now) {
    const c = this.cfg;
    const live = await this.idx.liveOrders(this.maker, c.token.covenantId);
    this.saveViews();
    const byId = new Map(live.map((o) => [o.id, o]));
    const timeout = c.safety.orderVisibleTimeoutSec * 1000;
    for (const [id, s] of Object.entries(this.state.orders)) {
      const o = byId.get(id);
      if (o) {
        s.amountLeft = o.amountLeft;
        s.value = o.value;
        if (s.pending) {
          const seen = s.pending.kind === 'place' || s.pending.kind === 'reread' || (s.pending.kind === 'amend' && o.txid === s.pending.txid && o.price === s.price);
          if (seen) s.pending = null;
          else if (now - s.pending.since > timeout) {
            this.log.info(`${short(id)}: the ${s.pending.kind} was not seen after ${c.safety.orderVisibleTimeoutSec} s; taking the indexer's view (price ${o.price})`);
            if (o.price !== s.price) s.quotedFair = 0; // unknown quote: the next cycle amends it to the target
            s.price = o.price;
            s.pending = null;
          }
        }
        continue;
      }
      // Not in the live list: ended, or not indexed yet.
      if (s.pending?.kind === 'place' && now - s.pending.since <= timeout) continue;
      const one = await this.idx.order(id);
      if (one?.live) {
        s.amountLeft = one.amountLeft;
        s.value = one.value;
        byId.set(id, one);
        continue;
      }
      if (!one && s.pending && now - s.pending.since <= timeout) continue;
      const status = one?.status ?? 'unknown to the indexer';
      this.log.info(`${short(id)} (${s.side} L${s.level}) ended: ${status}`);
      this.log.action({ type: 'ended', id, side: s.side, level: s.level, status });
      this.state.recent[id] = { side: s.side, level: s.level, endedAt: now };
      delete this.state.orders[id];
    }
    for (const [id, r] of Object.entries(this.state.recent)) if (now - r.endedAt > 3_600_000) delete this.state.recent[id];

    // An unknown order of the token on the bot's key gets a level past the ladder, so the planner
    // cancels it.
    for (const o of live) {
      if (this.state.orders[o.id] || !o.side || (o.contract !== 'KobAsk' && o.contract !== 'KobBid')) continue;
      this.log.info(`${short(o.id)}: a live ${o.side} of this token on the bot's key that the state file does not know: treated as a stale quote of the bot's own and cancelled`);
      this.log.action({ type: 'unknown-order', id: o.id, side: o.side, price: o.price, amountLeft: o.amountLeft });
      this.state.orders[o.id] = { side: o.side, level: 999, price: o.price, amount: o.amountLeft, quotedFair: 0, placedAt: now, pending: null, amountLeft: o.amountLeft, value: o.value, adopted: true };
    }
    return this.ordersNow();
  }

  /** The bot's orders from the state file, in planActions' shape. */
  ordersNow() {
    return Object.entries(this.state.orders).map(([id, s]) => ({
      id, side: s.side, level: s.level, price: s.price, quotedFair: s.quotedFair, amountLeft: s.amountLeft ?? s.amount, value: s.value ?? 0,
      pending: !!s.pending, pendingKind: s.pending?.kind ?? null,
    }));
  }

  /** Logs the fills of the bot's orders that the indexer reports and the bot has not logged yet. */
  async recordFills() {
    const items = await this.idx.fills(this.cfg.token.covenantId, 100);
    const lastId = this.state.lastFillId;
    if (lastId == null) {
      // The first run: start from the newest fill, do not replay history.
      this.state.lastFillId = items[0]?.id ?? 0;
      return;
    }
    const fresh = items.filter((f) => f.id > lastId).sort((a, b) => a.id - b.id);
    for (const f of fresh) {
      const mine = this.state.orders[f.covenant_id] ?? this.state.recent[f.covenant_id];
      if (!mine) continue;
      const amount = Number(f.amount ?? 0);
      const price = f.price == null ? null : Number(f.price);
      const tokens = amount / this.scale;
      const kas = price == null ? null : (amount * price) / this.scale / 1e8;
      this.log.info(`FILL ${mine.side === 'sell' ? 'sold' : 'bought'} ${fmtAmount(tokens, 6)} ${this.cfg.token.label ?? 'tokens'} at ${price == null ? '?' : fmtAmount(price / 1e8, 8)} KAS (${kas == null ? '?' : fmtAmount(kas, 4)} KAS), order ${short(f.covenant_id)} L${mine.level}, tx ${f.txid}`);
      this.log.fill({ id: f.covenant_id, side: mine.side, level: mine.level, amount: String(amount), price: price == null ? null : String(price), tokens, kas, txid: f.txid, daa: f.daa, eventId: f.id, payout: f.payout ?? null });
    }
    if (items.length) this.state.lastFillId = Math.max(lastId, items[0].id);
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
    const free = { kas: Math.max(0, bal.kasFree - Math.floor(c.quotes.kasReserve * 1e8)), tokens: bal.token.free };
    let actions = planActions(targets, orders, { fair: snap.fair, scale: this.scale, free, q });
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
      if ((a.kind === 'place' || a.kind === 'merge') && lastPlace && Date.now() - lastPlace < c.safety.placeGapSec * 1000) await wait(c.safety.placeGapSec * 1000 - (Date.now() - lastPlace));
      n++;
      const r = await this.run1(a);
      if (a.kind === 'place' || a.kind === 'merge') lastPlace = Date.now();
      if (a.kind === 'merge' && r.ok) break; // the asks are placed in the next cycle, from the merged UTXO
    }
  }

  /** One action through the CLI, with its bookkeeping. */
  async run1(a) {
    const c = this.cfg;
    const tok = c.token.covenantId;
    const desc = a.kind === 'place' ? `place ${a.side} L${a.level} ${fmtAmount(a.amount / this.scale, 6)} @ ${fmtAmount(a.price / 1e8, 8)} KAS`
      : a.kind === 'amend' ? `amend ${short(a.id)} ${a.side} L${a.level} ${fmtAmount(a.from / 1e8, 8)} -> ${fmtAmount(a.price / 1e8, 8)} KAS`
        : a.kind === 'cancel' ? `cancel ${short(a.id)} ${a.side} L${a.level}` : `merge ${c.token.label ?? 'token'}`;
    const r = a.kind === 'place' ? await this.kob.place(tok, a.side, a.amount, a.price)
      : a.kind === 'amend' ? await this.kob.amend(a.id, a.price)
        : a.kind === 'cancel' ? await this.kob.cancel(a.id) : await this.kob.merge(tok);
    const rec = { type: a.kind, ...a, status: r.ok ? (r.dryRun ? 'planned' : 'submitted') : 'failed', txid: r.txid ?? null, fee: r.fee ?? null, newIds: r.newIds ?? [], error: r.error ?? null };
    this.log.action(rec);
    if (!r.ok) {
      this.log.info(`${desc} (${a.reason}): FAILED ${r.error}`);
      // The order's state is unknown now: leave it alone until the next cycle has re-read it.
      if (a.id && this.state.orders[a.id]) this.state.orders[a.id].pending = { kind: 'reread', since: Date.now() };
      this.save();
      return r;
    }
    this.log.info(`${desc} (${a.reason}): ${r.dryRun ? 'planned (dry run)' : 'submitted'}${r.txid ? ` tx ${r.txid}` : ''}, fee ${fmtAmount(kasOf(r.fee), 8)} KAS`);
    if (r.dryRun) return r;
    addFee(this.state, a.kind, r.fee);
    const now = Date.now();
    if (a.kind === 'place') {
      const id = r.newIds[0];
      if (id) this.state.orders[id] = { side: a.side, level: a.level, price: a.price, amount: a.amount, quotedFair: a.quotedFair, placedAt: now, pending: { kind: 'place', since: now, txid: r.txid }, amountLeft: a.amount, value: 0 };
    } else if (a.kind === 'amend') {
      const s = this.state.orders[a.id];
      const inPlace = r.doc?.plan?.inPlace !== false && !r.newIds.length;
      if (inPlace) Object.assign(s, { price: a.price, quotedFair: a.quotedFair, pending: { kind: 'amend', since: now, txid: r.txid } });
      else {
        // A cancel-replace: the order has a new id.
        delete this.state.orders[a.id];
        this.state.recent[a.id] = { side: a.side, level: a.level, endedAt: now };
        const id = r.newIds[0];
        if (id) this.state.orders[id] = { ...s, price: a.price, quotedFair: a.quotedFair, placedAt: now, pending: { kind: 'place', since: now, txid: r.txid } };
        this.log.info(`${short(a.id)} was cancel-replaced (not in place): new id ${id}`);
      }
    } else if (a.kind === 'cancel') {
      const s = this.state.orders[a.id];
      if (s) s.pending = { kind: 'cancel', since: now, txid: r.txid };
    }
    this.save();
    return r;
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

  /** Retired tokens: cancel the bot's orders and merge the returned UTXOs. Never place or amend. */
  async retire(bal, stopping) {
    const now = Date.now();
    for (const id of this.cfg.retiredTokens) {
      const r = (this.state.retired[id] ??= { done: false, cancels: {} });
      const live = (await this.idx.liveOrders(this.maker, id)).map((o) => ({ ...o, cancelSentAt: r.cancels[o.id] ?? null }));
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
        const res = a.kind === 'cancel' ? await this.kob.cancel(a.id) : await this.kob.merge(id);
        const status = res.ok ? (res.dryRun ? 'planned' : 'submitted') : 'failed';
        this.log.action({ type: `retired-${a.kind}`, token: id, id: a.id ?? null, status, txid: res.txid ?? null, fee: res.fee ?? null, error: res.error ?? null });
        this.log.info(`retired ${short(id)}: ${a.kind}${a.id ? ` ${short(a.id)}` : ''}: ${res.ok ? `${status} tx ${res.txid ?? '-'}, fee ${fmtAmount(kasOf(res.fee ?? 0), 8)} KAS` : `FAILED ${res.error}`}`);
        if (res.ok && !res.dryRun) {
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
      quotes: after.map((o) => ({ id: o.id, side: o.side, level: o.level, price: o.price, amountLeft: o.amountLeft, value: o.value, quotedFair: o.quotedFair, pending: o.pendingKind })),
      funds: (this.funds ?? []).map((f) => ({ key: f.key, level: f.level, message: f.message })),
      fees: this.state.fees,
      dryRun: this.dryRun,
    };
  }
}

export { IDLE, QUOTING, STOPPING };
