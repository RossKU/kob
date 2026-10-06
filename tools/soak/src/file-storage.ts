// File-backed StorageLike for the bots' token tracker (run/state/tracker-<name>.json: genesis outputs and pre-run holdings the indexer
// never lists live only there). Kept free of `@/` imports so `node --test` can load it.
//
// The file is shared with other processes: `scripts/holdings.cjs --seed`, the balances / cancel CLIs and setup all read or write the
// same files while the bots run. So the in-memory copy is only a cache of the file:
//   - every read first checks the file (mtime, size, inode) and reloads it when it changed;
//   - every write re-reads the file first and replaces only the key being set (atomic: temp file + rename), so a key written by
//     someone else is carried over instead of being reverted by the next bot write.
// Two writers can still interleave between the read and the rename (no cross-process lock); the window is a few milliseconds and the
// loser's key is the same one the winner just wrote, never an unrelated one.
import { copyFileSync, readFileSync, renameSync, statSync, writeFileSync } from 'node:fs';
import { logger } from './log.ts';

const rootLog = logger('wallet');

export interface FileStorageOptions {
  /** where warnings go (default: the soak log) */
  warn?: (msg: string, fields?: Record<string, unknown>) => void;
}

export class FileStorage {
  private m: Record<string, string> = {};
  /** signature of the file state `m` reflects (null: not read yet) */
  private sig: string | null = null;
  /** the file on disk could not be parsed at the last look: `m` is the memory copy, and the file is saved aside before it is replaced */
  private unreadable = false;
  private warnedSig: string | null = null;
  private readonly warn: (msg: string, fields?: Record<string, unknown>) => void;
  private readonly path: string;

  constructor(path: string, o: FileStorageOptions = {}) {
    this.path = path;
    this.warn = o.warn ?? ((msg, f) => rootLog.warn(msg, f));
    this.refresh();
  }

  private signature(): string {
    try {
      const s = statSync(this.path);
      return `${s.mtimeMs}:${s.size}:${s.ino}`;
    } catch {
      return 'absent';
    }
  }

  /** reloads the file when it is not the one `m` reflects */
  private refresh(): void {
    const sig = this.signature();
    if (sig === this.sig) return;
    if (sig === 'absent') {
      this.m = {};
      this.sig = sig;
      this.unreadable = false;
      return;
    }
    try {
      const parsed = JSON.parse(readFileSync(this.path, 'utf8')) as unknown;
      if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) throw new Error('not a JSON object');
      const m: Record<string, string> = {};
      for (const [k, v] of Object.entries(parsed)) if (typeof v === 'string') m[k] = v;
      this.m = m;
      this.sig = sig;
      this.unreadable = false;
    } catch (e) {
      // torn or hand-edited file: keep serving the memory copy and look again on the next call (it may be a write in progress)
      this.unreadable = true;
      if (this.warnedSig !== sig) {
        this.warnedSig = sig;
        this.warn('token tracker file unreadable, keeping the in-memory copy', { path: this.path, error: e instanceof Error ? e.message : String(e) });
      }
    }
  }

  private persist(): void {
    if (this.unreadable) {
      // never replace a file we could not read without keeping it
      const aside = `${this.path}.unreadable-${Date.now()}`;
      try {
        copyFileSync(this.path, aside);
        this.warn('token tracker file replaced; the unreadable original is kept', { path: this.path, aside });
      } catch {
        /* the file vanished meanwhile: nothing to keep */
      }
    }
    const tmp = `${this.path}.${process.pid}.tmp`;
    writeFileSync(tmp, JSON.stringify(this.m));
    renameSync(tmp, this.path);
    this.unreadable = false;
    this.sig = this.signature();
  }

  getItem(k: string): string | null {
    this.refresh();
    return this.m[k] ?? null;
  }

  setItem(k: string, v: string): void {
    this.refresh();
    // a tracker store that loses more than half of its candidates at once is logged with the caller (pre-run holdings the indexer
    // never lists live only here: losing them hides the key's tokens from the bots)
    try {
      const before = this.m[k] ? (JSON.parse(this.m[k]) as { items?: unknown[] }).items?.length ?? 0 : 0;
      const after = (JSON.parse(v) as { items?: unknown[] }).items?.length ?? 0;
      if (before >= 4 && after * 2 < before) this.warn('token tracker store shrank', { path: this.path, before, after, stack: new Error().stack?.split('\n').slice(2, 8).join(' | ') });
    } catch {
      /* not a tracker record */
    }
    this.m[k] = v;
    this.persist();
  }

  removeItem(k: string): void {
    this.refresh();
    if (!(k in this.m)) return;
    delete this.m[k];
    this.persist();
  }
}
