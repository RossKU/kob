// What the tools do: reads through the CLI and the indexer; writes as plan, check, then send.

import { createHash, randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { Audit } from './audit.mjs';
import { Ledger, allowListed, bandCheck, capRefusals, killSwitchOn, today, tokenCapRefusals, tokenOutflows } from './guard.mjs';
import { Indexer } from './indexer.mjs';
import { Kob } from './kob.mjs';
import { big, kas, toSompi } from './units.mjs';
import { addressOfXOnly, pickFundingUtxo } from './wallet.mjs';

const RE = {
  id: /^[0-9a-fA-F]{64}$/,
  token: /^([0-9a-fA-F]{64}|[A-Za-z0-9]{2,12})$/,
  tokenAmount: /^\d+(\.\d+)?[A-Za-z][A-Za-z0-9]{0,11}$/,
  kasAmount: /^\d+(\.\d{1,8})?KAS$|^\d+sompi$/i,
  when: /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?Z|daa:\d+)$/,
};

/** Throws a refusal (shown to the agent) unless `v` matches. */
function need(re, v, what) {
  if (typeof v !== 'string' || !re.test(v)) throw new Refusal(`BAD_ARGUMENT: ${what} ${JSON.stringify(v)} does not read`);
  return v;
}

/** A free-text value for the CLI: one line, never starting with "-" (it would read as an option). */
function text(v, what, max = 1024) {
  if (typeof v !== 'string' || !v.length || v.length > max || /^-/.test(v) || /[\r\n\0]/.test(v)) {
    throw new Refusal(`BAD_ARGUMENT: ${what} must be one line of 1..${max} characters, not starting with "-"`);
  }
  return v;
}

export class Refusal extends Error {}

const hashArgs = (tool, args) => {
  const { confirm, ...rest } = args ?? {};
  const sorted = JSON.stringify(rest, Object.keys(rest).sort());
  return createHash('sha256').update(`${tool}\n${sorted}`).digest('hex');
};

/** The KAS a planned or sent transaction takes out of the wallet: max(fee, -net KAS); a merge's chain: its fee. */
export function spendOf(doc) {
  if (doc?.links) return { spend: big(doc.fee), fee: big(doc.fee), net: -big(doc.fee) };
  const fee = big(doc?.transaction?.fee);
  const net = big(doc?.transaction?.net?.kas);
  const out = -net > fee ? -net : fee;
  return { spend: out, fee, net };
}

export class KobService {
  constructor(cfg, { now = () => Date.now() } = {}) {
    this.cfg = cfg;
    this.now = now;
    this.kob = new Kob(cfg);
    this.indexer = new Indexer(cfg);
    this.ledger = new Ledger(join(cfg.dataDir, 'ledger.jsonl'));
    this.audit = new Audit(join(cfg.dataDir, 'audit.jsonl'));
    this.plans = new Map();
    this.queue = Promise.resolve();
    this.maker = null;
    this.features = { claim: false, delivery: false };
  }

  /** What this CLI build offers, read once from its help: `kob claim` and `--delivery` (bid delivery). */
  async detectFeatures() {
    const claim = await this.kob.exec(['claim', '--help'], { bare: true });
    const place = await this.kob.exec(['place', '--help'], { bare: true });
    this.features = {
      claim: claim.code === 0 && /\bclaim\b/.test(claim.stdout),
      delivery: place.code === 0 && /--delivery\b/.test(place.stdout),
    };
    return this.features;
  }

  /** Hides the key path from anything returned. */
  redact(s) {
    const k = this.cfg.keyFile;
    if (!k) return s;
    return [k, k.replace(/\\/g, '/'), k.replace(/\//g, '\\'), JSON.stringify(k).slice(1, -1)].reduce((t, p) => t.split(p).join('<key file>'), s);
  }

  /** Writes run one at a time, so the ledger and the plans see each other. */
  serial(fn) {
    const run = this.queue.then(fn, fn);
    this.queue = run.catch(() => {});
    return run;
  }

  // ---------- reads ----------

  async cli(args) {
    const r = await this.kob.exec(args);
    if (r.code !== 0 || !r.json) throw new Error(`kob ${args[0]}: exit ${r.code}: ${r.stderr || 'no output'}`);
    return r.json;
  }

  tokens({ search, all } = {}) {
    const a = ['tokens'];
    if (search) a.push(need(/^[A-Za-z0-9]{1,64}$/, search, 'search'));
    if (all) a.push('--all');
    return this.cli(a);
  }

  async book({ token, quote, depth, orders }) {
    if (orders && !quote) return this.indexer.get(`/v1/books/${await this.tokenId(token)}?depth=${Number(depth ?? 10)}&aggregate=false`);
    const a = ['book', need(RE.token, token, 'token')];
    if (quote) a.push('--quote', need(RE.token, quote, 'quote'));
    if (depth) a.push('--depth', String(depth));
    return this.cli(a);
  }

  balances() {
    return this.cli(['balances']);
  }

  orders({ token, all } = {}) {
    const a = ['orders'];
    if (token) a.push('--token', need(RE.token, token, 'token'));
    if (all) a.push('--all');
    return this.cli(a);
  }

  /** A covenant id for indexer routes: a ticker is looked up in `kob tokens` and must be unique. */
  async tokenId(token) {
    need(RE.token, token, 'token');
    if (RE.id.test(token)) return token.toLowerCase();
    const list = await this.tokens({ search: token });
    const hits = (list.tokens ?? []).filter((t) => String(t.ticker ?? '').toUpperCase() === token.toUpperCase());
    if (hits.length !== 1) throw new Refusal(`TOKEN_AMBIGUOUS: ${hits.length} tokens have the ticker ${token}; use the covenant id`);
    return hits[0].covenant_id;
  }

  async stats({ token }) {
    return this.indexer.get(`/v1/stats/${await this.tokenId(token)}`);
  }

  async candles({ token, interval = '1h', limit = 48 }) {
    const id = await this.tokenId(token);
    return this.indexer.get(`/v1/candles/${id}?interval=${encodeURIComponent(interval)}&limit=${Number(limit)}`);
  }

  async order({ id, events }) {
    need(RE.id, id, 'id');
    const view = await this.indexer.get(`/v1/orders/${id.toLowerCase()}`);
    if (!view) throw new Refusal(`NOT_FOUND: the indexer does not know the order ${id}`);
    return events ? { order: view, events: await this.indexer.get(`/v1/orders/${id.toLowerCase()}/events?limit=100`) } : view;
  }

  async makerKey() {
    if (!this.maker) this.maker = (await this.balances()).maker;
    return this.maker;
  }

  async status() {
    const day = today();
    const spent = this.ledger.spentOn(day);
    let wallet = null;
    try {
      const maker = await this.makerKey();
      wallet = { maker, address: addressOfXOnly(maker, this.cfg.network) };
    } catch (e) {
      wallet = { error: e.message };
    }
    const { caps } = this.cfg;
    return {
      wallet,
      killSwitch: killSwitchOn(this.cfg) ? 'ON: every write is refused (remove the STOP file to resume)' : 'off',
      today: { day, spentKas: kas(spent), leftKas: kas(caps.perDay > spent ? caps.perDay - spent : 0n),
        launches: this.ledger.launchesOn(day), launchesPerDay: this.cfg.limits.launchesPerDay,
        unknownOutcomes: this.ledger.unknownOn(day) },
      cli: { claim: this.features.claim ? 'kob claim: offered' : 'kob claim: not in this CLI build (kob_claim is not registered)',
        delivery: this.features.delivery ? '--delivery: offered' : '--delivery: not in this CLI build (kob_place_limit has no delivery argument)' },
    };
  }

  // ---------- writes ----------

  /** The CLI arguments of a write tool, after the checks that need no plan. Throws a Refusal. */
  async build(tool, a) {
    const amt = (v, w) => need(RE.tokenAmount, v, w);
    const px = (v, w) => need(RE.kasAmount, v, w);
    switch (tool) {
      case 'kob_place_limit': {
        const argv = ['place', 'limit', '--token', need(RE.token, a.token, 'token'), '--side', a.side, '--amount', amt(a.amount, 'amount'), '--price', px(a.price, 'price')];
        if (a.delivery) {
          if (!this.features.delivery) throw new Refusal('NOT_OFFERED: this kob CLI build has no --delivery');
          argv.push('--delivery', a.delivery);
        }
        if (a.tip) argv.push('--tip', px(a.tip, 'tip'));
        if (a.gtd) argv.push('--gtd', need(RE.when, a.gtd, 'gtd'));
        if (a.day) argv.push('--day');
        if (a.crossing) argv.push('--crossing', a.crossing);
        if (a.decimals != null) argv.push('--decimals', String(a.decimals));
        return { argv, band: { side: a.side, price: toSompi(a.price), token: a.token } };
      }
      case 'kob_replace': {
        const argv = ['replace', need(RE.id, a.id, 'id').toLowerCase()];
        if (a.price) argv.push('--price', px(a.price, 'price'));
        if (a.amount) argv.push('--amount', amt(a.amount, 'amount'));
        if (a.tip) argv.push('--tip', px(a.tip, 'tip'));
        if (a.renew) argv.push('--renew');
        if (argv.length === 2) throw new Refusal('BAD_ARGUMENT: name what changes: price, amount, tip or renew');
        let band = null;
        if (a.price) {
          const o = await this.indexer.tryGet(`/v1/orders/${a.id.toLowerCase()}`);
          if (o?.side === 1 || o?.side === 2) band = { side: o.side === 1 ? 'sell' : 'buy', price: toSompi(a.price), token: o.token };
        }
        return { argv, band };
      }
      case 'kob_cancel':
        return { argv: ['cancel', need(RE.id, a.id, 'id').toLowerCase()] };
      case 'kob_take': {
        if ((a.max_price == null) === (a.min_price == null)) {
          throw new Refusal('PRICE_BOUND_REQUIRED: give exactly one of max_price (you buy from a sell order) or min_price (you sell into a buy order)');
        }
        // the token is pinned: the CLI refuses an order whose node-verified state trades another token
        const tok = need(RE.id, a.token, 'token (the covenant id of the token the order trades)').toLowerCase();
        const argv = ['take', need(RE.id, a.id, 'id').toLowerCase(), '--token', tok, '--amount', amt(a.amount, 'amount')];
        if (a.max_price) argv.push('--max-price', px(a.max_price, 'max_price'));
        else argv.push('--min-price', px(a.min_price, 'min_price'));
        return { argv, band: { side: a.max_price ? 'buy' : 'sell', price: toSompi(a.max_price ?? a.min_price), token: tok } };
      }
      case 'kob_merge':
        return { argv: ['merge', need(RE.token, a.token, 'token')] };
      case 'kob_claim': {
        if (!this.features.claim) throw new Refusal('NOT_OFFERED: this kob CLI build has no kob claim');
        const argv = ['claim', need(RE.id, a.id, 'id').toLowerCase()];
        if (a.merge && a.sell_price) throw new Refusal('BAD_ARGUMENT: merge and sell_price exclude each other');
        if (a.merge) argv.push('--merge');
        if (a.sell_price) {
          argv.push('--sell', '--price', px(a.sell_price, 'sell_price'));
          if (a.sell_amount) argv.push('--amount', amt(a.sell_amount, 'sell_amount'));
        }
        if (a.end) argv.push('--end');
        return { argv };
      }
      case 'kob_send': {
        const to = String(a.to ?? '').trim().toLowerCase();
        if (!allowListed(this.cfg, to)) {
          throw new Refusal(`NOT_ALLOW_LISTED: ${to || '(no address)'} is not on the withdrawal allow-list (withdrawTo in the config); kob_send pays only the primary wallet`);
        }
        const asset = a.asset.toUpperCase() === 'KAS' ? 'KAS' : need(RE.token, a.asset, 'asset');
        const amount = asset === 'KAS' ? px(a.amount, 'amount') : amt(a.amount, 'amount');
        return { argv: ['send', asset, to, amount] };
      }
      case 'kob_token_create':
        return this.buildCreate(a);
      default:
        throw new Refusal(`UNKNOWN_TOOL: ${tool}`);
    }
  }

  async buildCreate(a) {
    const argv = ['token', 'create', '--name', text(a.name, 'name', 64), '--ticker', need(/^[A-Z0-9]{2,12}$/, a.ticker, 'ticker'),
      '--decimals', String(a.decimals), '--supply', need(/^[1-9]\d{0,38}$/, a.supply, 'supply (base units)')];
    if (a.program) argv.push('--program', a.program);
    if (a.description) argv.push('--description', text(a.description, 'description'));
    if (a.website) argv.push('--website', text(a.website, 'website', 256));
    if (a.image) argv.push('--image', text(a.image, 'image', 256));
    const carrier = a.carrier ? toSompi(px(a.carrier, 'carrier')) : 1_000_000_000n;
    argv.push('--carrier', String(carrier));
    return { argv, create: { carrier } };
  }

  /** The funding UTXO of a creation: given, or the smallest that covers the carrier and 1 KAS of fees. */
  async fundingFor(a, carrier) {
    const maker = await this.makerKey();
    if (a.fund_utxo) {
      const m = /^([0-9a-f]{64}):(\d+):(\d+)$/i.exec(a.fund_utxo);
      if (!m) throw new Refusal('BAD_ARGUMENT: fund_utxo is TXID:INDEX:AMOUNT_SOMPI');
      return { txid: m[1].toLowerCase(), index: Number(m[2]), amount: BigInt(m[3]), maker };
    }
    if (!this.cfg.node || !/^wss?:\/\/[^/]+:(18110|18210)\b/.test(this.cfg.node)) {
      throw new Refusal('NO_FUNDING: set node to a JSON wRPC URL (ws://host:18210) in the config, or pass fund_utxo');
    }
    const u = await pickFundingUtxo(this.cfg.node, maker, this.cfg.network, carrier + 100_000_000n);
    if (!u) throw new Refusal(`NO_FUNDING: no confirmed plain UTXO of at least ${kas(carrier + 100_000_000n)} KAS`);
    return { ...u, maker };
  }

  /** Runs `kob token create` (dry or --yes) into `dir`; returns { code, stderr, doc } with doc built from the files. */
  async runCreate(argv, fund, dir, yes) {
    const full = [...argv, '--fund-utxo', `${fund.txid}:${fund.index}:${fund.amount}:${fund.maker}`, '--out-dir', dir, yes ? '--yes' : '--dry-run'];
    const r = await this.kob.exec(full, { json: false, indexer: false });
    if (r.code !== 0) return { code: r.code, stderr: r.stderr || r.stdout.slice(-600), doc: null };
    const read = (f) => (existsSync(join(dir, f)) ? JSON.parse(readFileSync(join(dir, f), 'utf8')) : null);
    const prep = read('preparation_tx.json');
    const gen = read('genesis_tx.json');
    const creation = read('creation.json');
    if (!prep || !gen) return { code: 1, stderr: 'the creation files were not written', doc: null };
    const outs = prep.outputs.map((o) => BigInt(o.value));
    const change = outs.slice(1).reduce((s, v) => s + v, 0n);
    const prepFee = fund.amount - outs.reduce((s, v) => s + v, 0n);
    const genFee = outs[0] - gen.outputs.reduce((s, o) => s + BigInt(o.value), 0n);
    const fee = prepFee + genFee;
    const spend = fund.amount - change;
    const doc = {
      action: `token create ${argv[argv.indexOf('--ticker') + 1]}`,
      network: this.cfg.network,
      plan: { covenant_id: creation?.covenant_id ?? null, preparation_txid: creation?.preparation_txid ?? null, genesis_txid: creation?.genesis_txid ?? null,
        funding: `${fund.txid}:${fund.index}`, carrier_kas: kas(spend - fee) },
      issues: [],
      transaction: { fee: String(fee), net: { kas: String(-spend) } },
      new_covenant_ids: creation?.covenant_id ? [creation.covenant_id] : [],
      result: yes ? { status: 'submitted', txid: creation?.genesis_txid, txids: [creation?.preparation_txid, creation?.genesis_txid] } : null,
      text: r.stdout.split(/\r?\n/).filter((l) => l.trim()).slice(-12),
    };
    return { code: 0, doc };
  }

  /** A dry run of a write: { ok, doc, spend, fee, error }. */
  async dryRun(tool, built, a, planDir) {
    if (built.create) {
      const fund = built.fund ?? await this.fundingFor(a, built.create.carrier);
      built.fund = fund;
      const r = await this.runCreate(built.argv, fund, planDir, false);
      if (r.code !== 0) return { ok: false, error: `CLI_REFUSED (exit ${r.code}): ${r.stderr}` };
      return { ok: true, doc: r.doc, ...spendOf(r.doc) };
    }
    const extra = this.cfg.cli.priorityFee ? ['--priority-fee'] : [];
    const r = await this.kob.exec([...built.argv, ...extra, '--dry-run']);
    const errs = (r.json?.issues ?? []).filter((i) => i.severity === 'error');
    if (r.code !== 0 || errs.length || !r.json) {
      // A take's bound refusal is an issue in the JSON, or only the stderr text when the fill could not be built either.
      const bound = errs.length ? '' : takeRefusalCode(r.stderr);
      return { ok: false, doc: r.json, error: `CLI_REFUSED (exit ${r.code}): ${errs.map((i) => `${i.code} ${i.message}`).join('; ') || `${bound}${r.stderr || 'no output'}`}` };
    }
    return { ok: true, doc: r.json, ...spendOf(r.json) };
  }

  summary(doc, s) {
    return {
      action: doc?.action,
      network: doc?.network,
      spendKas: kas(s.spend),
      feeKas: kas(s.fee),
      netKas: kas(s.net),
      netTokens: doc?.transaction?.net?.tokens ?? {},
      issues: doc?.issues ?? [],
      inputs: doc?.transaction?.inputs,
      outputs: doc?.transaction?.outputs,
      plan: doc?.plan,
      newCovenantIds: doc?.new_covenant_ids ?? [],
      ...(doc?.links ? { merge: { utxosBefore: doc.utxos_before, utxosAfter: doc.utxos_after, transactions: doc.transactions } } : {}),
      ...(doc?.text ? { text: doc.text } : {}),
    };
  }

  /**
   * A write tool: plan (dry run) and check; without `confirm` in confirm mode, return the plan and its id; with the
   * matching `confirm` (or in auto mode), plan again, check again and send. `estimate` stops after the checks.
   */
  write(tool, args, { estimate = false } = {}) {
    return this.serial(() => this.writeNow(tool, args, estimate));
  }

  async writeNow(tool, args, estimate) {
    const log = (outcome, extra = {}) => this.audit.log({ tool, args: this.redactArgs(args), outcome, ...extra });
    try {
      if (!estimate && killSwitchOn(this.cfg)) throw new Refusal(`KILL_SWITCH: the kill switch file ${this.cfg.killSwitchFile.split(/[\\/]/).pop()} exists; every write is refused until it is removed`);
      for (const [k, p] of this.plans) if (k !== args.confirm && this.now() > p.expires) this.plans.delete(k);
      const hash = hashArgs(tool, args);
      let plan = null;
      if (args.confirm && !estimate) {
        plan = this.plans.get(args.confirm);
        if (!plan) throw new Refusal('PLAN_UNKNOWN: no such plan (already used, expired, or the server restarted); call again without confirm');
        if (plan.hash !== hash) throw new Refusal('PLAN_MISMATCH: the arguments differ from the plan; call again without confirm to plan them');
        if (this.now() > plan.expires) {
          this.plans.delete(args.confirm);
          throw new Refusal('PLAN_EXPIRED: plan again');
        }
      }
      const built = await this.build(tool, args);
      if (plan?.fund) built.fund = plan.fund;
      if (built.create && this.ledger.launchesOn(today()) >= this.cfg.limits.launchesPerDay) {
        throw new Refusal(`LAUNCH_CAP: ${this.cfg.limits.launchesPerDay} token creations per UTC day, ${this.ledger.launchesOn(today())} today`);
      }
      const band = built.band ? await this.band(built.band) : null;
      if (band && !band.ok) throw new Refusal(band.reason);
      const planId = plan?.id ?? randomBytes(6).toString('hex');
      const planDir = join(this.cfg.dataDir, 'creations', `${planId}-plan-${this.now()}`);
      const d = await this.dryRun(tool, built, args, planDir);
      if (!d.ok) throw new Refusal(d.error);
      const day = today();
      const spentToday = this.ledger.spentOn(day);
      d.tokens = built.create ? {} : tokenOutflows(d.doc);
      const caps = [...capRefusals(d, this.cfg.caps, spentToday),
        ...await tokenCapRefusals(d.tokens, this.cfg.caps.tokens, (c) => this.ledger.tokensOutOn(day, c), (c) => this.freeHolding(c))];
      const limits = { perTxKas: kas(this.cfg.caps.perTx), perDayKas: kas(this.cfg.caps.perDay), spentTodayKas: kas(spentToday), maxFeeKas: kas(this.cfg.caps.maxFee) };
      const summary = { ...this.summary(d.doc, d), tokensOut: d.tokens, priceBand: band?.note ?? null, limits };
      if (caps.length) throw new Refusal(caps.join('; '), { cause: summary });
      if (estimate) {
        log('estimate', { spend: String(d.spend), fee: String(d.fee) });
        return { status: 'estimate', ...summary };
      }
      if (!plan && this.cfg.mode === 'confirm') {
        const expires = this.now() + this.cfg.planTtlSec * 1000;
        this.plans.set(planId, { id: planId, tool, hash, expires, spend: d.spend, fund: built.fund });
        log('plan', { planId, spend: String(d.spend), fee: String(d.fee) });
        return {
          status: 'plan',
          planId,
          expiresAt: new Date(expires).toISOString(),
          next: `Nothing is signed. To send this, call ${tool} again with the same arguments and confirm: "${planId}".`,
          ...summary,
        };
      }
      if (plan && d.spend > plan.spend + this.cfg.caps.maxFee) {
        throw new Refusal(`PLAN_CHANGED: it now takes ${kas(d.spend)} KAS, the confirmed plan ${kas(plan.spend)} KAS; plan again`);
      }
      if (plan) this.plans.delete(plan.id);
      return await this.send(tool, built, args, planId, d, day, summary, log);
    } catch (e) {
      if (e instanceof Refusal) {
        log('refused', { reason: e.message });
        return { status: 'refused', reason: this.redact(e.message), ...(e.cause ? { plan: e.cause } : {}) };
      }
      log('error', { error: this.redact(String(e.message)) });
      throw e;
    }
  }

  async send(tool, built, args, planId, d, day, summary, log) {
    this.ledger.reserve(planId, day, d.spend, { tool, ...(Object.keys(d.tokens ?? {}).length ? { tokens: d.tokens } : {}) });
    let doc = null;
    let failure = null;
    let ambiguous = false;
    if (built.create) {
      const r = await this.runCreate(built.argv, built.fund, join(this.cfg.dataDir, 'creations', `${planId}-sent`), true);
      if (r.code !== 0) { failure = `exit ${r.code}: ${r.stderr}`; ambiguous = /genesis|resume/i.test(r.stderr) || !sentNothing(r.stderr); } else doc = r.doc;
    } else {
      const extra = this.cfg.cli.priorityFee ? ['--priority-fee'] : [];
      const r = await this.kob.exec([...built.argv, ...extra, '--yes']);
      doc = r.json;
      // Only a refusal the CLI states came before the submit (or the node's rejection) releases the spend; a lost reply,
      // a crash or an unreadable result counts until the UTC day rolls.
      if (r.timedOut) { failure = 'the CLI timed out: the outcome is unknown, check kob_orders / kob_balances'; ambiguous = true; } else if (r.code !== 0) {
        failure = `exit ${r.code}: ${r.stderr || 'no output'}`;
        ambiguous = Boolean(doc?.links?.some((l) => l.status === 'submitted')) || !sentNothing(r.stderr);
      } else if (!doc?.links && doc?.result?.status !== 'submitted') { failure = `not submitted: ${r.stderr || 'no result'}`; ambiguous = !sentNothing(r.stderr); }
    }
    const txids = doc?.links ? doc.links.filter((l) => l.status === 'submitted').map((l) => l.txid) : doc?.result?.txids ?? (doc?.result?.txid ? [doc.result.txid] : []);
    if (failure) {
      const counted = ambiguous || txids.length > 0;
      if (counted) {
        // the planned transaction's id, to look up by hand
        const known = txids.length ? txids : [doc?.transaction?.txid].filter(Boolean);
        this.ledger.settle(planId, d.spend, known, 'unknown');
      } else this.ledger.release(planId, failure);
      log('failed', { planId, error: this.redact(failure), txids });
      return { status: 'failed', planId, reason: this.redact(failure), txids, counted,
        ...(counted ? { note: 'the outcome is unknown: the spend counts toward today\'s cap; check kob_orders / kob_balances before trying again' } : {}) };
    }
    const actual = spendOf(doc);
    const spend = built.create ? d.spend : actual.spend;
    this.ledger.settle(planId, spend, txids);
    log('submitted', { planId, txids, spend: String(spend), fee: String(actual.fee) });
    return {
      status: 'submitted',
      planId,
      txids,
      newCovenantIds: doc?.new_covenant_ids ?? doc?.result?.covenant_ids ?? [],
      spendKas: kas(spend),
      feeKas: kas(actual.fee),
      action: summary.action,
      spentTodayKas: kas(this.ledger.spentOn(day)),
    };
  }

  /** The wallet's free holding of a token as confirmed on the node (kob balances), base units; null when not confirmed. */
  async freeHolding(cov) {
    const row = ((await this.balances().catch(() => null))?.tokens ?? []).find((t) => String(t.token).toLowerCase() === cov);
    return row?.free_verified === true ? big(row.free) : null;
  }

  /** The price band of a limit price or take bound, with the token's decimals for books whose scale is capped at 10^9. */
  async band({ side, price, token }) {
    const book = await this.bookFor(token);
    let decimals = null;
    const scale = book?.scale != null ? BigInt(book.scale) : null;
    if (scale != null && scale < 1_000_000_000n) decimals = scale.toString().length - 1;
    else if (scale != null && book?.token) {
      const list = await this.tokens({ search: book.token }).catch(() => null);
      decimals = (list?.tokens ?? []).find((t) => t.covenant_id === book.token)?.decimals ?? null;
    }
    return bandCheck({ side, price, book, decimals, bandPct: this.cfg.limits.priceBandPct });
  }

  async bookFor(token) {
    try {
      return await this.cli(['book', token, '--depth', '1']);
    } catch {
      return null;
    }
  }

  redactArgs(a) {
    return JSON.parse(this.redact(JSON.stringify(a ?? {})));
  }
}

/**
 * The CLI's last error line says nothing was sent: a refusal before signing or submitting, a failed engine validation, or
 * the node's explicit rejection. Anything else after `--yes` started (a lost reply, a socket error) may have been sent.
 */
export function sentNothing(stderr) {
  const lines = String(stderr ?? '').split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  const last = [...lines].reverse().find((l) => /^error\b/i.test(l)) ?? lines.at(-1) ?? '';
  return /nothing signed|nothing submitted|nothing sent|not signed: pass --yes|engine validation failed|node rejected the request/i.test(last);
}

/** "TAKE_PRICE_ABOVE_MAX: " or "TAKE_PRICE_BELOW_MIN: " from the CLI's stderr text, else "". */
export function takeRefusalCode(stderr) {
  if (/TAKE_PRICE_ABOVE_MAX|above your --max-price/.test(stderr ?? '')) return 'TAKE_PRICE_ABOVE_MAX: ';
  if (/TAKE_PRICE_BELOW_MIN|below your --min-price/.test(stderr ?? '')) return 'TAKE_PRICE_BELOW_MIN: ';
  return '';
}

export function ensureDataDir(cfg) {
  mkdirSync(cfg.dataDir, { recursive: true });
}
