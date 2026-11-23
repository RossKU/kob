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
const NOISE = /blocks are contended|^dry run: nothing signed/;

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

  /** One CLI run: { code, docs, json (the first document), stderr }. */
  exec(args) {
    const argv = [...this.prefixArgs, ...args, ...this.common()];
    return new Promise((resolve) => {
      const child = spawn(this.cfg.kobCli, argv, { env: this.env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
      let out = '';
      let err = '';
      child.stdout.on('data', (d) => { out += d; });
      child.stderr.on('data', (d) => { err += d; });
      const timer = setTimeout(() => { child.kill(); err += '\nerror: timed out'; }, this.cfg.cli.timeoutSec * 1000);
      child.on('error', (e) => { clearTimeout(timer); resolve({ code: -1, docs: [], json: null, stderr: `cannot run ${this.cfg.kobCli}: ${e.message}` }); });
      child.on('close', (code) => {
        clearTimeout(timer);
        let docs = [];
        try { docs = parseJsonStream(out); } catch { /* not JSON */ }
        const stderr = err.split(/\r?\n/).filter((l) => l.trim() && !NOISE.test(l)).join(' | ').slice(0, 600);
        resolve({ code: code ?? -1, docs, json: docs[0] ?? null, stderr });
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
    if (this.cfg.cli.preflight || this.dryRun) {
      const plan = await this.exec([...args, ...extra, '--dry-run']);
      const errs = (plan.json?.issues ?? []).filter((i) => i.severity === 'error');
      if (plan.code !== 0 || errs.length) {
        return { ok: false, error: `plan refused (exit ${plan.code}): ${errs.map((i) => `${i.code} ${i.message}`).join('; ') || plan.stderr || 'no output'}`, doc: plan.json };
      }
      plannedFee = Number(plan.json?.transaction?.fee ?? plan.json?.fee ?? 0);
      if (this.dryRun) {
        return { ok: true, dryRun: true, txid: null, fee: plannedFee, newIds: plan.json?.new_covenant_ids ?? [], doc: plan.json };
      }
    }
    const r = await this.exec([...args, ...extra, '--yes']);
    // A failed --yes may have been sent all the same (a lost reply, a node that relays and answers
    // with an error): `sentFee` is its planned fee, to count, unless the CLI says nothing was sent.
    const sentFee = /nothing signed|nothing submitted|nothing sent|engine validation failed/i.test(r.stderr) ? null
      : Number(r.json?.transaction?.fee ?? r.json?.fee ?? 0) || plannedFee || null;
    if (r.code !== 0) return { ok: false, error: `exit ${r.code}: ${r.stderr || 'no output'}`, doc: r.json, sentFee };
    const doc = r.json;
    if (doc?.links) {
      // Merge: a summary with one link per transaction.
      const links = doc.links.filter((l) => l.status === 'submitted');
      return { ok: true, txid: links.map((l) => l.txid).join(','), txids: links.map((l) => l.txid), fee: Number(doc.fee ?? 0), newIds: [], doc };
    }
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

  /** A GTD limit order: `gtd` is the expiry as a UTC time (gtdArg), null for a 90-day GTC. */
  place(tokenId, side, amountBase, priceSompi, gtd = null) {
    return this.sign(['place', 'limit', '--token', tokenId, '--side', side, '--amount', `${amountBase}base`, '--price', `${priceSompi}sompi`, ...(gtd ? ['--gtd', gtd] : [])]);
  }

  /** An amend in place to a new price and, with `gtd`, a new expiry. */
  amend(id, priceSompi, gtd = null) {
    return this.sign(['replace', id, '--price', `${priceSompi}sompi`, ...(gtd ? ['--gtd', gtd] : [])]);
  }

  cancel(id) {
    return this.sign(['cancel', id]);
  }

  /**
   * Cancels every order of a saved page of indexer views (`kob cancel --view`: checked on the node,
   * no indexer). Returns { ok, ids (cancels sent or planned), fee, error }.
   */
  async cancelViews(file) {
    const r = await this.exec(['cancel', '--view', file, ...(this.cfg.cli.priorityFee ? ['--priority-fee'] : []), this.dryRun ? '--dry-run' : '--yes']);
    const sent = r.docs.filter((d) => this.dryRun || d?.result?.status === 'submitted');
    const ids = sent.map((d) => /^cancel ([0-9a-f]{64})/.exec(d.action ?? '')?.[1]).filter(Boolean);
    const fee = sent.reduce((s, d) => s + Number(d.result?.fee ?? d.transaction?.fee ?? 0), 0);
    return { ok: r.code === 0, ids, fee, txids: sent.map((d) => d.result?.txid).filter(Boolean), error: r.code === 0 ? null : `exit ${r.code}: ${r.stderr || 'no output'}` };
  }

  /**
   * Whether the order of a saved view still sits at the view's UTXO on the node: a dry run of
   * `kob cancel --view` (no indexer, nothing signed). 'live', 'gone' (refused as stale: the UTXO is
   * spent, the order ended or moved) or 'unknown' (the node did not answer).
   */
  async viewLive(file) {
    const r = await this.exec(['cancel', '--view', file, '--dry-run']);
    if (r.code === 0 && r.docs.some((d) => /^cancel [0-9a-f]{64}/.test(d?.action ?? ''))) return 'live';
    return /stale view/.test(r.stderr) ? 'gone' : 'unknown';
  }

  merge(tokenId) {
    return this.sign(['merge', tokenId]);
  }
}
