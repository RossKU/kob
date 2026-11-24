// The kob CLI as a child process: --json, --dry-run before --yes, exit code first, the key only as
// a file path or KOB_KEY.

import { spawn } from 'node:child_process';
import { join } from 'node:path';

/** Splits a stream of concatenated JSON documents (`kob cancel A B` prints one per id). */
export function parseJsonStream(text) {
  const docs = [];
  let depth = 0;
  let start = -1;
  let inStr = false;
  let esc = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inStr) {
      if (esc) esc = false;
      else if (ch === '\\') esc = true;
      else if (ch === '"') inStr = false;
      continue;
    }
    if (ch === '"') inStr = true;
    else if (ch === '{' || ch === '[') {
      if (depth++ === 0) start = i;
    } else if ((ch === '}' || ch === ']') && depth > 0 && --depth === 0) {
      docs.push(JSON.parse(text.slice(start, i + 1)));
    }
  }
  return docs;
}

/** The `--gtd` argument of a time in ms: UTC to the second, `2026-11-20T12:34:56Z`. */
export const gtdArg = (ms) => `${new Date(Math.floor(ms / 1000) * 1000).toISOString().slice(0, 19)}Z`;

// Stderr lines that are progress notes, not errors.
const NOISE = /^dry run: nothing signed/;
// The CLI's congestion note (relay-fee KOB transactions may wait): not an error, but it goes to
// the log (R7T-1), through `onNote`.
const NOTE = /blocks are contended/;

/** A long stderr cut in the middle: the last line, where the CLI says what failed, stays (T-13). */
export const cutMiddle = (s, n) => (s.length <= n ? s : `${s.slice(0, Math.floor(n / 4))} .. ${s.slice(s.length - (n - Math.floor(n / 4) - 4))}`);

export class Kob {
  /** opts: `dryRun` never signs. `prefixArgs` go before the command (tests). */
  constructor(cfg, { dryRun = false, prefixArgs = [] } = {}) {
    this.cfg = cfg;
    this.dryRun = dryRun;
    this.prefixArgs = prefixArgs;
    this.env = { ...process.env, KOB_STATE_DIR: join(cfg.dataDir, 'kob-state') };
  }

  common() {
    const a = ['--network', this.cfg.network, '--json'];
    if (this.cfg.node) a.push('--node', this.cfg.node);
    if (this.cfg.indexer) a.push('--indexer', this.cfg.indexer);
    if (this.cfg.keyFile) a.push('--key-file', this.cfg.keyFile);
    return a;
  }

  /** One CLI run: { code, docs, json (the first document), stdout, stderr }. `bare`: no common options. */
  exec(args, { bare = false } = {}) {
    const argv = [...this.prefixArgs, ...args, ...(bare ? [] : this.common())];
    return new Promise((resolve) => {
      const child = spawn(this.cfg.kobCli, argv, { env: this.env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
      let out = '';
      let err = '';
      child.stdout.on('data', (d) => { out += d; });
      child.stderr.on('data', (d) => { err += d; });
      const timer = setTimeout(() => { child.kill(); err += '\nerror: timed out'; }, this.cfg.cli.timeoutSec * 1000);
      child.on('error', (e) => { clearTimeout(timer); resolve({ code: -1, docs: [], json: null, stdout: '', stderr: `cannot run ${this.cfg.kobCli}: ${e.message}` }); });
      child.on('close', (code) => {
        clearTimeout(timer);
        let docs = [];
        try { docs = parseJsonStream(out); } catch { /* not JSON */ }
        const lines = err.split(/\r?\n/).filter((l) => l.trim() && !NOISE.test(l));
        for (const l of lines.filter((x) => NOTE.test(x))) this.onNote?.(l.trim());
        const stderr = cutMiddle(lines.filter((l) => !NOTE.test(l)).join(' | '), 600);
        resolve({ code: code ?? -1, docs, json: docs[0] ?? null, stdout: out, stderr });
      });
    });
  }

  /**
   * Plans with --dry-run, then signs with --yes unless dry. Returns { ok, dryRun, txid, fee,
   * newIds, doc, error }.
   */
  async sign(args) {
    const extra = this.cfg.cli.priorityFee ? ['--priority-fee'] : [];
    let plannedFee = null;
    let planDoc = null;
    if (this.cfg.cli.preflight || this.dryRun) {
      const plan = await this.exec([...args, ...extra, '--dry-run']);
      const errs = (plan.json?.issues ?? []).filter((i) => i.severity === 'error');
      if (plan.code !== 0 || errs.length) {
        return { ok: false, error: `plan refused (exit ${plan.code}): ${errs.map((i) => `${i.code} ${i.message}`).join('; ') || plan.stderr || 'no output'}`, doc: plan.json };
      }
      // A dry run whose answer does not read is no pass (R8T-3): nothing was signed.
      if (!plan.json) return { ok: false, error: `plan unreadable (exit 0, no JSON document): ${plan.stderr || 'no output'}`, doc: null };
      plannedFee = Number(plan.json?.transaction?.fee ?? plan.json?.fee ?? 0);
      planDoc = plan.json;
      if (this.dryRun) {
        return { ok: true, dryRun: true, txid: null, fee: plannedFee, newIds: plan.json?.new_covenant_ids ?? [], doc: plan.json };
      }
    }
    const r = await this.exec([...args, ...extra, '--yes']);
    // A failed --yes may have been sent all the same (a lost reply, a node that relays and answers
    // with an error): `sentFee` is its planned fee, to count, unless the CLI says nothing was sent.
    const sentFee = /nothing signed|nothing submitted|nothing sent|engine validation failed/i.test(r.stderr) ? null
      : Number(r.json?.transaction?.fee ?? r.json?.fee ?? 0) || plannedFee || null;
    // `doc` names what may have gone out (its covenant ids and states): the --yes run's plan, else the dry run's.
    if (r.code !== 0) return { ok: false, error: `exit ${r.code}: ${r.stderr || 'no output'}`, doc: r.json ?? planDoc, sentFee };
    const doc = r.json;
    if (doc?.links) {
      // Merge: a summary with one link per transaction.
      const links = doc.links.filter((l) => l.status === 'submitted');
      return { ok: true, txid: links.map((l) => l.txid).join(','), txids: links.map((l) => l.txid), fee: Number(doc.fee ?? 0), newIds: [], doc };
    }
    // Exit 0 with an answer that does not read may have been sent (R8T-3): it counts as sent, its
    // outcome unknown, with the dry run's document (the in-flight rules decide).
    if (!doc) return { ok: false, error: `unreadable answer after --yes (exit 0): ${r.stderr || 'no output'}`, doc: planDoc, sentFee: plannedFee || 1 };
    if (doc?.result?.status !== 'submitted') return { ok: false, error: `not submitted: ${r.stderr || 'no result'}`, doc, sentFee };
    return { ok: true, txid: doc.result.txid, fee: Number(doc.result.fee ?? doc.transaction?.fee ?? 0), newIds: doc.new_covenant_ids ?? [], doc };
  }

  /** The wallet in sompi and base units: `token` is the row of tokenId, `tokens` every row. */
  async balances(tokenId) {
    const r = await this.exec(['balances']);
    if (r.code !== 0 || !r.json?.kas) throw new Error(`kob balances: exit ${r.code}: ${r.stderr}`);
    const rowOf = (row) => ({ free: Number(row?.free ?? 0), inOrders: Number(row?.in_orders ?? 0), freeUtxos: Number(row?.free_utxos ?? 0), decimals: row?.decimals ?? null, freeVerified: row?.free_verified ?? null });
    const row = (r.json.tokens ?? []).find((t) => t.token === tokenId);
    return {
      tokens: Object.fromEntries((r.json.tokens ?? []).map((t) => [t.token, rowOf(t)])),
      maker: r.json.maker,
      kasFree: Number(r.json.kas.free),
      kasTotal: Number(r.json.kas.total),
      token: rowOf(row),
    };
  }

  /**
   * A GTD limit order: `gtd` is the expiry as a UTC time (gtdArg), null for a 90-day GTC. It names
   * the config's decimals: the CLI needs them for a token whose decimals only the indexer names,
   * and refuses them when another source names others (R2-T3). `kob replace` reads the order's own
   * scale and takes no --decimals.
   */
  placeArgs(tokenId, side, amountBase, priceSompi, gtd = null) {
    const dec = this.cfg.token?.decimals;
    return ['place', 'limit', '--token', tokenId, '--side', side, '--amount', `${amountBase}base`, '--price', `${priceSompi}sompi`,
      ...(dec != null ? ['--decimals', String(dec)] : []), ...(gtd ? ['--gtd', gtd] : []),
      // Whole-fill asks: the minimum fill is the whole amount, so only a full fill takes it.
      ...(side === 'sell' && this.cfg.quotes?.askPartialFills === false ? ['--min-fill', `${amountBase}base`] : [])];
  }

  place(tokenId, side, amountBase, priceSompi, gtd = null) {
    return this.sign(this.placeArgs(tokenId, side, amountBase, priceSompi, gtd));
  }

  /** Whether the CLI would plan this placement now (a dry run, nothing signed): { ok, error }. */
  async probePlace(tokenId, side, amountBase, priceSompi, gtd = null) {
    const r = await this.exec([...this.placeArgs(tokenId, side, amountBase, priceSompi, gtd), '--dry-run']);
    const errs = (r.json?.issues ?? []).filter((i) => i.severity === 'error');
    return r.code === 0 && !errs.length ? { ok: true } : { ok: false, error: `exit ${r.code}: ${errs.map((i) => `${i.code} ${i.message}`).join('; ') || r.stderr || 'no output'}` };
  }

  /** The CLI's version line (`kob --version`), null when it does not answer. */
  async version() {
    const r = await this.exec(['--version'], { bare: true });
    return r.code === 0 ? r.stdout.trim() || null : null;
  }

  /** An amend in place to a new price and, with `gtd`, a new expiry. */
  amend(id, priceSompi, gtd = null) {
    return this.sign(['replace', id, '--price', `${priceSompi}sompi`, ...(gtd ? ['--gtd', gtd] : [])]);
  }

  cancel(id) {
    return this.sign(['cancel', id]);
  }

  /**
   * `kob recover` over a file of the bot's own order records (R2-T1). Each order is looked up on the
   * node at the script its state makes; a partial fill is found only at an amount named by an
   * `--amount-left` hint (one number or a list per id) or reached by the CLI's bounded search. No
   * indexer is read. `cancel` also cancels every live one (--yes, or --dry-run when dry). Returns { ok, orders (the report's entries), error }.
   */
  async recover(file, { maker = null, hints = {}, cancel = false } = {}) {
    const a = ['recover', '--from', file, '--network', this.cfg.network];
    if (this.cfg.node) a.push('--node', this.cfg.node);
    if (maker) a.push('--maker', maker);
    for (const [id, h] of Object.entries(hints)) for (const n of [h].flat()) if (Number.isSafeInteger(n) && n > 0) a.push('--amount-left', `${id}=${n}`);
    if (cancel) {
      a.push('--cancel');
      if (this.cfg.keyFile) a.push('--key-file', this.cfg.keyFile);
      if (this.cfg.cli.priorityFee) a.push('--priority-fee');
      a.push(this.dryRun ? '--dry-run' : '--yes');
    }
    const r = await this.exec(a, { bare: true });
    const report = r.docs.find((d) => Array.isArray(d?.orders));
    if (!report) return { ok: false, orders: [], error: `kob recover: exit ${r.code}: ${r.stderr || 'no report'}` };
    return { ok: true, orders: report.orders, error: r.code === 0 ? null : `kob recover: exit ${r.code}: ${r.stderr || 'a cancel failed'}` };
  }

  merge(tokenId) {
    return this.sign(['merge', tokenId]);
  }
}
