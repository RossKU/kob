// JSON-lines logging to stdout (the supervisor writes it to a rotated log file) and a tiny counter registry flushed to run/stats/*.json.
import { mkdirSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

type Level = 'debug' | 'info' | 'warn' | 'error';
const LEVELS: Record<Level, number> = { debug: 10, info: 20, warn: 30, error: 40 };
const minLevel = LEVELS[(process.env.SOAK_LOG_LEVEL as Level) ?? 'info'] ?? 20;

const replacer = (_k: string, v: unknown) => (typeof v === 'bigint' ? v.toString() : v instanceof Error ? { message: v.message, stack: v.stack?.split('\n').slice(0, 4).join(' | ') } : v);

export interface Logger {
  debug(msg: string, f?: Record<string, unknown>): void;
  info(msg: string, f?: Record<string, unknown>): void;
  warn(msg: string, f?: Record<string, unknown>): void;
  error(msg: string, f?: Record<string, unknown>): void;
  child(src: string): Logger;
}

export function logger(src: string): Logger {
  const emit = (lvl: Level, msg: string, f?: Record<string, unknown>) => {
    if (LEVELS[lvl] < minLevel) return;
    process.stdout.write(JSON.stringify({ t: new Date().toISOString(), lvl, src, msg, ...(f ?? {}) }, replacer) + '\n');
  };
  return {
    debug: (m, f) => emit('debug', m, f),
    info: (m, f) => emit('info', m, f),
    warn: (m, f) => emit('warn', m, f),
    error: (m, f) => emit('error', m, f),
    child: (s) => logger(`${src}.${s}`),
  };
}

export const errText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** Monotonic counters (by name) and gauges, written atomically as JSON for the report. */
export class Stats {
  readonly counters: Record<string, number> = {};
  readonly gauges: Record<string, unknown> = {};
  private readonly path: string;
  constructor(dir: string, name: string) {
    mkdirSync(dir, { recursive: true });
    this.path = join(dir, `${name}.json`);
  }
  inc(name: string, by = 1): void {
    this.counters[name] = (this.counters[name] ?? 0) + by;
  }
  set(name: string, v: unknown): void {
    this.gauges[name] = v;
  }
  flush(): void {
    const tmp = this.path + '.tmp';
    const mem = process.memoryUsage();
    writeFileSync(tmp, JSON.stringify({ ts: Date.now(), pid: process.pid, rssMb: Math.round(mem.rss / 1e6), counters: this.counters, gauges: this.gauges }, replacer, 2));
    renameSync(tmp, this.path);
  }
}
