// What the tools do: reads through the CLI and the indexer; writes as plan, check, then send.

import { createHash, randomBytes } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { Audit } from './audit.mjs';
import { DEFAULT_REF_DEPTH_KAS, Ledger, allowListed, bandCheck, bandReference, bookReference, capRefusals, killSwitchOn, sideOfKind, statePrice, tipRefusals, today, tokenCapRefusals, tokenOutflows } from './guard.mjs';
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

/**
 * A free-text value for the CLI: one line, never starting with "-" (it would read as an option).
 */
function text(v, what, max = 1024) {
  if (typeof v !== 'string' || !v.length || v.length > max || /^-/.test(v) || /[\r\n\0]/.test(v)) {
    throw new Refusal(`BAD_ARGUMENT: ${what} must be one line of 1..${max} characters, not starting with "-"`);
  }
  return v;
}

/** Throws a refusal unless `v` is one of `list` (enum arguments reach the CLI as they are). */
function oneOf(v, list, what) {
  if (!list.includes(v)) throw new Refusal(`BAD_ARGUMENT: ${what} ${JSON.stringify(v)} is not one of ${list.join(', ')}`);
  return v;
}

/** Throws a refusal unless `v` is a whole number lo..hi; returns it as text. */
function whole(v, lo, hi, what) {
  if (!Number.isInteger(v) || v < lo || v > hi) throw new Refusal(`BAD_ARGUMENT: ${what} ${JSON.stringify(v)} is not a whole number ${lo}..${hi}`);
  return String(v);
}

export class Refusal extends Error {}

/** The farthest a creator lock may open: about 10 years (3,653 days) from now. */
export const MAX_LOCK_MS = 3_653 * 86_400_000;

/**
 * `lock_until` as the CLI takes it: +N days, hours or minutes, a date (UTC), or unix milliseconds
 * (13 digits: seconds and microseconds are refused). It must be at most MAX_LOCK_MS away. The CLI
 * checks it again, and that it lies in the future.
 */
export function lockUntil(v, now = Date.now()) {
  const s = need(/^(\+\d{1,7}[dhm]|\d{4}-\d{2}-\d{2}(T\d{2}:\d{2}(:\d{2})?Z)?|\d{13})$/, v, 'lock_until (+90d, a date YYYY-MM-DD or unix milliseconds, 13 digits)');
  const rel = /^\+(\d+)([dhm])$/.exec(s);
  const step = { d: 86_400_000, h: 3_600_000, m: 60_000 };
  const at = rel ? now + Number(rel[1]) * step[rel[2]] : /^\d{13}$/.test(s) ? Number(s) : Date.parse(s.length === 10 ? `${s}T00:00:00Z` : s);
  if (!Number.isFinite(at)) throw new Refusal(`BAD_ARGUMENT: lock_until ${s} is not a date`);
  if (at - now > MAX_LOCK_MS) throw new Refusal(`BAD_ARGUMENT: lock_until ${s} is more than 10 years away`);
  return s;
}

const hashArgs = (tool, args) => {
  const { confirm, ...rest } = args ?? {};
  const sorted = JSON.stringify(rest, Object.keys(rest).sort());
  return createHash('sha256').update(`${tool}\n${sorted}`).digest('hex');
};

/**
 * The KAS a planned or sent transaction takes out of the wallet: max(fee, -net KAS).
 * For a merge's chain it is the fee.
 */
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
    this.nodeTimeoutMs = 20_000; // the funding lookup on the node (R3T-4)
    this.maker = null;
    this.features = { claim: false, delivery: false };
  }

  /**
   * What this CLI build offers, read once from its help: `kob claim` and `--delivery` (bid
   * delivery).
   */
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
        const argv = ['place', 'limit', '--token', need(RE.token, a.token, 'token'), '--side', oneOf(a.side, ['buy', 'sell'], 'side'), '--amount', amt(a.amount, 'amount'), '--price', px(a.price, 'price')];
        if (a.delivery) {
          if (!this.features.delivery) throw new Refusal('NOT_OFFERED: this kob CLI build has no --delivery');
          argv.push('--delivery', oneOf(a.delivery, ['each', 'collected', 'one-utxo'], 'delivery'));
        }
        if (a.tip) argv.push('--tip', px(a.tip, 'tip'));
        if (a.gtd) argv.push('--gtd', need(RE.when, a.gtd, 'gtd'));
        if (a.day) argv.push('--day');
        if (a.crossing) argv.push('--crossing', oneOf(a.crossing, ['auction', 'limit', 'reject'], 'crossing'));
        if (a.decimals != null) argv.push('--decimals', whole(a.decimals, 0, 9, 'decimals'));
        // The band judges the all-in price: a buy pays price + tip, a sell receives price - tip (T-9).
        const price = toSompi(a.price);
        const tip = a.tip ? toSompi(a.tip) : 0n;
        return { argv, band: { side: a.side, price: a.side === 'buy' ? price + tip : price > tip ? price - tip : 0n, token: a.token }, tips: true };
      }
      case 'kob_replace': {
        const argv = ['replace', need(RE.id, a.id, 'id').toLowerCase()];
        if (a.price) argv.push('--price', px(a.price, 'price'));
        if (a.amount) argv.push('--amount', amt(a.amount, 'amount'));
        if (a.tip) argv.push('--tip', px(a.tip, 'tip'));
        if (a.renew) argv.push('--renew');
        if (argv.length === 2) throw new Refusal('BAD_ARGUMENT: name what changes: price, amount, tip or renew');
        // A new price, tip or amount is checked after the dry run, against the order as the CLI
        // checked it on the node (replaceChecks), never against the indexer's view of it. Only a
        // bare renew moves no value.
        return { argv, reprice: a.price || a.tip || a.amount ? { priceGiven: Boolean(a.price) } : null };
      }
      case 'kob_cancel':
        return { argv: ['cancel', need(RE.id, a.id, 'id').toLowerCase()] };
      case 'kob_take': {
        if ((a.max_price == null) === (a.min_price == null)) {
          throw new Refusal('PRICE_BOUND_REQUIRED: give exactly one of max_price (you buy from a sell order) or min_price (you sell into a buy order)');
        }
        // The token is pinned: the CLI refuses an order whose node-verified state trades another
        // token.
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
        return { argv, claimSell: a.sell_price ? { price: toSompi(a.sell_price) } : null };
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
    const px = (v, w) => need(RE.kasAmount, v, w);
    const argv = ['token', 'create', '--name', text(a.name, 'name', 64), '--ticker', need(/^[A-Z0-9]{2,12}$/, a.ticker, 'ticker'),
      '--decimals', whole(a.decimals, 0, 18, 'decimals'), '--supply', need(/^[1-9]\d{0,38}$/, a.supply, 'supply (base units)')];
    if (a.program) argv.push('--program', oneOf(a.program, ['public-mint', '3x3'], 'program'));
    if (a.description) argv.push('--description', text(a.description, 'description'));
    if (a.website) argv.push('--website', text(a.website, 'website', 256));
    if (a.image) argv.push('--image', text(a.image, 'image', 256));
    const carrier = a.carrier ? toSompi(px(a.carrier, 'carrier')) : 1_000_000_000n;
    argv.push('--carrier', String(carrier));
    // After creation: keep everything (default), or sell on a curve created in the genesis (its own
    // UTXO, its custody, a kept share).
    // lock_until locks the kept share on chain (the curve's kept part, else keep_percent of the
    // supply, default all of it).
    const lock = a.lock_until === undefined ? null : lockUntil(a.lock_until);
    if (lock) argv.push('--lock-until', lock);
    let outputs = 1n;
    if (a.sell === 'curve') {
      argv.push('--sell', 'curve');
      const keep = a.keep_percent ?? 0;
      if (!(Number.isFinite(keep) && keep >= 0 && keep < 100 && Number.isInteger(Math.round(keep * 100)) && Math.abs(keep * 100 - Math.round(keep * 100)) < 1e-9)) {
        throw new Refusal('BAD_ARGUMENT: keep_percent is 0 to 99.99, two decimals at most');
      }
      if (keep > 0) argv.push('--keep', String(keep));
      if (a.levels !== undefined) argv.push('--levels', need(/^\d{1,4}$/, String(a.levels), 'levels'));
      if (a.total) argv.push('--total', px(a.total, 'total'));
      if (a.multiple) argv.push('--multiple', need(/^\d{1,6}(\.\d{1,3})?$/, a.multiple, 'multiple'));
      outputs = keep > 0 ? 3n : 2n;
      if (lock && keep === 0) throw new Refusal('BAD_ARGUMENT: lock_until locks the kept share: give keep_percent');
    } else if (a.levels !== undefined || a.total || a.multiple || (a.keep_percent !== undefined && !lock)) {
      throw new Refusal('BAD_ARGUMENT: keep_percent, levels, total and multiple go with sell: "curve" (keep_percent also with lock_until)');
    } else if (lock && a.keep_percent !== undefined) {
      const keep = a.keep_percent;
      if (!(Number.isFinite(keep) && keep > 0 && keep <= 100 && Math.abs(keep * 100 - Math.round(keep * 100)) < 1e-9)) {
        throw new Refusal('BAD_ARGUMENT: keep_percent is above 0 and at most 100, two decimals at most');
      }
      argv.push('--keep', String(keep));
      if (keep < 100) outputs = 2n;
    }
    return { argv, create: { carrier, locked: carrier * outputs } };
  }

  /**
   * The funding UTXO of a creation: given, or the smallest that covers the carriers and 1 KAS of
   * fees.
   */
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
    const u = await pickFundingUtxo(this.cfg.node, maker, this.cfg.network, carrier + 100_000_000n, this.nodeTimeoutMs);
    if (!u) throw new Refusal(`NO_FUNDING: no confirmed plain UTXO of at least ${kas(carrier + 100_000_000n)} KAS`);
    return { ...u, maker };
  }

  /**
   * Runs `kob token create` (dry or --yes) into `dir`; returns { code, stderr, doc } with doc built
   * from the files.
   */
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
      const fund = built.fund ?? await this.fundingFor(a, built.create.locked);
      built.fund = fund;
      const r = await this.runCreate(built.argv, fund, planDir, false);
      if (r.code !== 0) return { ok: false, error: `CLI_REFUSED (exit ${r.code}): ${r.stderr}` };
      return { ok: true, doc: r.doc, ...spendOf(r.doc) };
    }
    const extra = this.cfg.cli.priorityFee ? ['--priority-fee'] : [];
    const r = await this.kob.exec([...built.argv, ...extra, '--dry-run']);
    const errs = (r.json?.issues ?? []).filter((i) => i.severity === 'error');
    if (r.code !== 0 || errs.length || !r.json) {
      // A take's bound refusal is an issue in the JSON, or only the stderr text when the fill could
      // not be built either.
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
   * A write tool: plan (dry run) and check.
   * Without `confirm` in confirm mode, it returns the plan and its id. With the matching `confirm`
   * (or in auto mode), it plans again, checks again and sends. `estimate` stops after the checks.
   */
  write(tool, args, { estimate = false } = {}) {
    return this.serial(() => this.writeNow(tool, args, estimate));
  }

  async writeNow(tool, args, estimate) {
    const log = (outcome, extra = {}) => this.audit.log({ tool, args: this.redactArgs(args), outcome, ...extra });
    let planDir = null;
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
      let band = built.band ? await this.band(built.band) : null;
      if (band && !band.ok) throw new Refusal(band.reason);
      const planId = plan?.id ?? randomBytes(6).toString('hex');
      planDir = join(this.cfg.dataDir, 'creations', `${planId}-plan-${this.now()}`);
      const d = await this.dryRun(tool, built, args, planDir);
      if (!d.ok) throw new Refusal(d.error);
      const day = today();
      const spentToday = this.ledger.spentOn(day);
      d.tokens = built.create ? {} : tokenOutflows(d.doc);
      const tips = [];
      if (built.tips) {
        const value = big(d.doc?.plan?.request?.value) || -big(d.doc?.transaction?.net?.kas);
        for (const o of d.doc?.plan?.states ?? []) tips.push(...tipRefusals(o, value, this.cfg.caps.tips));
      }
      if (built.reprice) {
        const r = await this.replaceChecks(built.reprice, d.doc);
        band = r.band;
        tips.push(...r.tips);
        for (const [cov, n] of Object.entries(r.held)) if (BigInt(n) > big(d.tokens[cov])) d.tokens[cov] = n;
        // A buy priced up commits its whole escrow at the new price, as a new buy would (T-3).
        if (r.commit > d.spend) d.spend = d.commit = r.commit;
      }
      if (built.claimSell) {
        const r = await this.claimChecks(built.claimSell, args.id, d.doc);
        band = r.band;
        d.tokens = r.tokens;
      }
      if (tips.length) throw new Refusal(tips.join('; '));
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
    } finally {
      // An estimate keeps no creation files.
      if (estimate && planDir) rmSync(planDir, { recursive: true, force: true });
    }
  }

  async send(tool, built, args, planId, d, day, summary, log) {
    // The STOP file may have appeared while the dry run ran.
    if (killSwitchOn(this.cfg)) throw new Refusal(`KILL_SWITCH: the kill switch file ${this.cfg.killSwitchFile.split(/[\\/]/).pop()} exists; every write is refused until it is removed`);
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
      // Only a refusal the CLI states came before the submit releases the spend. A lost reply, a
      // node's rejection of the submit, a crash or an unreadable result counts until the UTC day
      // rolls.
      if (r.timedOut) { failure = 'the CLI timed out: the outcome is unknown, check kob_orders / kob_balances'; ambiguous = true; } else if (r.code !== 0) {
        failure = `exit ${r.code}: ${r.stderr || 'no output'}`;
        ambiguous = Boolean(doc?.links?.some((l) => l.status === 'submitted')) || !sentNothing(r.stderr);
      } else if (!doc?.links && doc?.result?.status !== 'submitted') { failure = `not submitted: ${r.stderr || 'no result'}`; ambiguous = !sentNothing(r.stderr); }
    }
    const txids = doc?.links ? doc.links.filter((l) => l.status === 'submitted').map((l) => l.txid) : doc?.result?.txids ?? (doc?.result?.txid ? [doc.result.txid] : []);
    if (failure) {
      const counted = ambiguous || txids.length > 0;
      if (counted) {
        // The planned transaction's id, to look up by hand.
        const known = txids.length ? txids : [doc?.transaction?.txid].filter(Boolean);
        this.ledger.settle(planId, d.spend, known, 'unknown');
      } else this.ledger.release(planId, failure);
      log('failed', { planId, error: this.redact(failure), txids });
      return { status: 'failed', planId, reason: this.redact(failure), txids, counted,
        ...(counted ? { note: 'the outcome is unknown: the spend counts toward today\'s cap; check kob_orders / kob_balances before trying again' } : {}) };
    }
    const actual = spendOf(doc);
    // A raised buy's escrow stays counted after the send: the transaction itself only pays the fee.
    const spend = built.create ? d.spend : d.commit != null && d.commit > actual.spend ? d.commit : actual.spend;
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

  /**
   * The wallet's free holding of a token as confirmed on the node (kob balances), base units; null
   * when not confirmed.
   */
  async freeHolding(cov) {
    const row = ((await this.balances().catch(() => null))?.tokens ?? []).find((t) => String(t.token).toLowerCase() === cov);
    return row?.free_verified === true ? big(row.free) : null;
  }

  /**
   * The price band of a limit price or take bound, with the token's decimals for books whose scale
   * is capped at 10^9. `priceScale`: the price is per that many base units (bandCheck). A book that
   * cannot be read refuses: a band that was never checked must not pass as one (L23).
   */
  async band({ side, price, priceScale = null, token }) {
    if (this.cfg.limits.priceBandPct == null && !this.cfg.refs?.size) return bandCheck({ bandPct: null });
    const book = await this.bookFor(token);
    const cov = String(book?.token ?? (RE.id.test(String(token)) ? token : '')).toLowerCase();
    const op = this.cfg.refs?.get(cov) ?? null;
    const bandPct = op?.bandPct ?? this.cfg.limits.priceBandPct;
    if (bandPct == null) return bandCheck({ bandPct });
    if (!book) return { ok: false, reason: `PRICE_BAND_UNCHECKED: the book of ${token} could not be read, so the price band cannot be checked; try again later` };
    let decimals = null;
    const scale = book?.scale != null ? BigInt(book.scale) : null;
    if (scale != null && scale < 1_000_000_000n) decimals = scale.toString().length - 1;
    else if (scale != null && book?.token) {
      const list = await this.tokens({ search: book.token }).catch(() => null);
      decimals = (list?.tokens ?? []).find((t) => t.covenant_id === book.token)?.decimals ?? null;
    }
    // The operator's price, sompi per whole token, as sompi per `scale` base units.
    const opRef = op && scale != null && decimals != null ? { num: op.sompi * scale, den: 10n ** BigInt(decimals), what: 'the operator\'s refPrice' } : null;
    // The book's reference needs refDepthKas on its fills and on its resting side (R2-T2): one fill
    // two keys print, or a few KAS of orders, cannot set it.
    const [rows, fills, maker] = cov ? await Promise.all([
      this.indexer.tryGet(`/v1/books/${cov}?depth=200&aggregate=false`),
      this.indexer.tryGet(`/v1/fills?token=${cov}&limit=200`),
      this.makerKey().catch(() => null),
    ]) : [null, null, null];
    const b = bookReference({ side, rows, fills: fills?.items, scale, depth: this.cfg.refDepth ?? toSompi(DEFAULT_REF_DEPTH_KAS), maker });
    const ref = bandReference({ side, op: opRef, book: b.ref });
    return bandCheck({ side, price, priceScale, ref, scale, decimals, bandPct, why: b.why });
  }

  /**
   * The checks of an amended resting order (E-M4, T-1, T-3), the same as for a new order. The side
   * and the token come from the dry run's `current` and `replacement` states, which the CLI read
   * from the order's UTXO on the node, never from the indexer's view of the order.
   *
   * Prices are all-in: a sell receives price - tip, a buy pays price + tip, so a tip raise is a
   * price move. The all-in price is always banded. A move against the maker (a sell's all-in
   * price down or its amount up, a buy's all-in price up; any change when the plan has no
   * comparable current state) needs a band that was checked, unless the operator turned the band
   * off. Such a move is a new order of what the order holds: a sell counts its whole custody
   * under the token caps, a buy its whole KAS (the plan's `value`) under the KAS caps. The tip
   * caps apply to the replacement. Returns { band, held: { cov: base units }, commit, tips }.
   */
  async replaceChecks(_reprice, doc) {
    const rep = doc?.plan?.replacement;
    const cur = doc?.plan?.current;
    const kind = String(rep?.kind ?? '');
    const side = sideOfKind(kind);
    const token = String(rep?.state?.tokenCovId ?? '').toLowerCase();
    if (!side || !RE.id.test(token) || /^0+$/.test(token)) {
      throw new Refusal(`PRICE_BAND_UNCHECKED: the plan names ${kind ? `a ${kind}` : 'no replacement order'}, whose new terms this server cannot check against a KAS book; cancel it and place a new order`);
    }
    const allIn = (st) => {
      const p = statePrice(st);
      const tip = big(st?.tip);
      return p == null ? null : side === 'sell' ? p - tip : p + tip;
    };
    const comparable = cur?.kind === kind;
    const was = comparable ? allIn(cur.state) : null;
    const now = allIn(rep.state);
    const more = side === 'sell' && (!comparable || big(rep.state.amountLeft) > big(cur.state?.amountLeft));
    const against = was == null || now == null || more || (side === 'sell' ? now < was : now > was);
    const scale = /^\d+$/.test(String(rep.state.scale ?? '')) ? rep.state.scale : null;
    if (now == null || now <= 0n || scale == null) {
      throw new Refusal(`PRICE_BAND_UNCHECKED: the plan does not name the ${side}'s all-in price and scale; cancel it and place a new order`);
    }
    const band = await this.band({ side, price: now, priceScale: scale, token });
    // An unchecked band is never ok unless the operator turned it off (R2-T2), so any priced
    // amend, toward the maker or against it, needs a checked band here.
    if (!band.ok) throw new Refusal(band.reason);
    const held = {};
    let commit = 0n;
    if (side === 'sell' && against) {
      const left = rep.state.amountLeft;
      if (!/^\d+$/.test(String(left ?? ''))) throw new Refusal('PRICE_BAND_UNCHECKED: the plan does not name the sell\'s custody; cancel it and place a new order');
      held[token] = String(left);
    }
    if (side === 'buy' && against) {
      if (!/^\d+$/.test(String(doc.plan.value ?? ''))) throw new Refusal('PRICE_BAND_UNCHECKED: the plan does not name the buy\'s KAS; cancel it and place a new order');
      commit = BigInt(doc.plan.value);
    }
    return { band, held, commit, tips: tipRefusals(rep, doc.plan.value, this.cfg.caps.tips) };
  }

  /**
   * The checks of a claim that sells what a collected buy bought (T-2, sec-ops M17): the sell
   * price is banded, and the claimed tokens count under the token caps, as a new sell of them.
   * They are not the wallet's UTXOs, so the plan's net effect leaves them out: the tokens out are
   * the plan's `bought` (read on the node) less what of the token the net effect brings back
   * (or plus what joins from the wallet). The claim plan does not name the token, so it is taken
   * from what the CLI read on the node (R2-T4): the order's state in a dry run of `kob replace
   * --renew`, else the one token of the net effect. The indexer's view of the order is never used,
   * and the net effect must name no other token.
   */
  async claimChecks({ price }, id, doc) {
    const bought = String(doc?.plan?.bought ?? '');
    if (!/^\d+$/.test(bought)) throw new Refusal('PRICE_BAND_UNCHECKED: the claim plan does not name the bought amount; claim without sell_price, then place the sell');
    const net = doc?.transaction?.net?.tokens ?? {};
    const netIds = Object.keys(net).map((c) => c.toLowerCase());
    const read = await this.kob.exec(['replace', String(id).toLowerCase(), '--renew', '--dry-run']);
    const cur = read.json?.plan?.current;
    const onNode = sideOfKind(cur?.kind) === 'buy' ? String(cur.state?.tokenCovId ?? '').toLowerCase() : '';
    const token = RE.id.test(onNode) && !/^0+$/.test(onNode) ? onNode : netIds.length === 1 ? netIds[0] : '';
    if (!RE.id.test(token) || netIds.some((c) => c !== token)) {
      throw new Refusal('PRICE_BAND_UNCHECKED: the token of the claimed order is not confirmed on the node (or the plan moves another token); claim without sell_price, then place the sell');
    }
    const band = await this.band({ side: 'sell', price, token });
    if (!band.ok) throw new Refusal(band.reason);
    const back = big(Object.entries(net).find(([c]) => c.toLowerCase() === token)?.[1]);
    const out = BigInt(bought) - back;
    return { band, tokens: out > 0n ? { [token]: String(out) } : {} };
  }

  async bookFor(token) {
    try {
      // Deep enough for the depth reference (10 KAS of bids).
      return await this.cli(['book', token, '--depth', '50']);
    } catch {
      return null;
    }
  }

  redactArgs(a) {
    return JSON.parse(this.redact(JSON.stringify(a ?? {})));
  }
}

/**
 * True when the CLI's last error line says nothing was sent: a refusal before signing or
 * submitting, a failed engine validation, or a node's rejection of a read before signing.
 * Anything else after `--yes` started (a lost reply, a socket error) may have been sent.
 * A failed submit (`submit: ...`) may have been sent too, even when the node says it rejected
 * the transaction: a dishonest node can relay it and answer with an error (E-L3).
 */
export function sentNothing(stderr) {
  const lines = String(stderr ?? '').split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  const last = [...lines].reverse().find((l) => /^error\b/i.test(l)) ?? lines.at(-1) ?? '';
  if (/\bsubmit:/i.test(last)) return false;
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
