// Incident log run/incidents.jsonl: one JSON line `{ts, invariant, severity, subject, detail}` per violation, appended ONCE per
// (invariant, subject) for the life of the soak (the existing file is read back at start). No `@/` imports (unit-tested).
import { appendFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';
import type { Violation } from './types.ts';

export interface Incident {
  ts: string;
  invariant: string;
  severity: string;
  subject: string;
  detail: Record<string, unknown>;
}

const replacer = (_k: string, v: unknown) => (typeof v === 'bigint' ? v.toString() : v);

export function readIncidents(path: string): Incident[] {
  if (!existsSync(path)) return [];
  const out: Incident[] = [];
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    if (!line.trim()) continue;
    try {
      out.push(JSON.parse(line) as Incident);
    } catch {
      /* torn line */
    }
  }
  return out;
}

export class IncidentLog {
  private readonly seen = new Set<string>();
  private readonly path: string;
  private readonly onNew: (i: Incident) => void;
  constructor(path: string, onNew: (i: Incident) => void = () => {}) {
    this.path = path;
    this.onNew = onNew;
    mkdirSync(dirname(path), { recursive: true });
    for (const i of readIncidents(path)) this.seen.add(`${i.invariant}|${i.subject}`);
  }

  has(invariant: string, subject: string): boolean {
    return this.seen.has(`${invariant}|${subject}`);
  }

  /** appends the violation unless its (invariant, subject) was reported before; true when it is new */
  report(v: Violation, now = new Date()): boolean {
    const k = `${v.invariant}|${v.subject}`;
    if (this.seen.has(k)) return false;
    this.seen.add(k);
    const inc: Incident = { ts: now.toISOString(), invariant: v.invariant, severity: v.severity, subject: v.subject, detail: v.detail };
    appendFileSync(this.path, JSON.stringify(inc, replacer) + '\n');
    this.onNew(inc);
    return true;
  }
}
