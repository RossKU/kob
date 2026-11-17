// The kob CLI as a child process, as tools/mm-bot/src/kob.mjs drives it: --json, --dry-run before --yes, exit code
// first, the key only as --key-file or KOB_KEY. No shell: every argument is its own argv entry.

import { spawn } from 'node:child_process';
import { join } from 'node:path';

/** Splits concatenated JSON documents (`kob cancel A B` prints one per id). */
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

// stderr lines that are progress notes, not errors
const NOISE = /blocks are contended|^dry run: nothing signed/;

export class Kob {
  constructor(cfg) {
    this.cfg = cfg;
    this.env = { ...process.env, KOB_STATE_DIR: join(cfg.dataDir, 'kob-state') };
  }

  /** The options every command gets; `json: false` for `token create`, which has no --json. */
  common({ json = true, key = true, indexer = true } = {}) {
    const a = ['--network', this.cfg.network];
    if (json) a.push('--json');
    if (this.cfg.node) a.push('--node', this.cfg.node);
    if (indexer && this.cfg.indexer) a.push('--indexer', this.cfg.indexer);
    if (key && this.cfg.keyFile) a.push('--key-file', this.cfg.keyFile);
    return a;
  }

  /** One run: { code, docs, json (the first document), stdout, stderr, timedOut }. */
  exec(args, opts = {}) {
    const argv = [...this.cfg.kobCliArgs, ...args, ...(opts.bare ? [] : this.common(opts))];
    return new Promise((resolve) => {
      const child = spawn(this.cfg.kobCli, argv, { env: this.env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
      let out = '';
      let err = '';
      let timedOut = false;
      child.stdout.on('data', (d) => { out += d; });
      child.stderr.on('data', (d) => { err += d; });
      const timer = setTimeout(() => { timedOut = true; child.kill(); }, this.cfg.cli.timeoutSec * 1000);
      child.on('error', (e) => {
        clearTimeout(timer);
        resolve({ code: -1, docs: [], json: null, stdout: '', stderr: `cannot run the kob CLI: ${e.message}`, timedOut });
      });
      child.on('close', (code) => {
        clearTimeout(timer);
        let docs = [];
        if (opts.json !== false) {
          try { docs = parseJsonStream(out); } catch { /* not JSON */ }
        }
        const stderr = err.split(/\r?\n/).filter((l) => l.trim() && !NOISE.test(l)).join(' | ').slice(0, 1500);
        resolve({ code: code ?? -1, docs, json: docs[0] ?? null, stdout: out, stderr: timedOut ? `${stderr} | timed out` : stderr, timedOut });
      });
    });
  }
}
