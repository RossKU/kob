// Why the soak's children restarted: classifies every exit in run/logs/supervisor.log since the supervisor's latest start.
//   requested      an operator restart (run/RESTART-<name>, scripts/restart-child.ps1) or a hold (run/HOLD-<name>)
//   start failure  the child exited within START_FAIL_SEC of its start (a missing bundle / binary, a bad config): a deploy step, not a crash
//   crash          any other exit the supervisor did not ask for
//
//   node scripts/restarts.mjs [run/logs/supervisor.log]     (JSON per child; scripts/status.ps1 prints it as a table)
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** an exit this soon after the start is a start failure */
export const START_FAIL_SEC = 5;
/** an exit this soon after `restart requested` / `hold` of that child is the requested one */
const REQUEST_WINDOW_MS = 15_000;

/**
 * @param {string[]} lines supervisor.log lines (JSON)
 * @returns {Record<string, { requested: number; startFailures: number; crashes: number; lastCrash: { at: string; upSec: number } | null; lastStartFailure: string | null }>}
 */
export function classifyExits(lines) {
  const events = [];
  for (const l of lines) {
    if (!l.trim()) continue;
    try {
      events.push(JSON.parse(l));
    } catch {
      /* a torn line */
    }
  }
  // only the supervisor's latest run: its children's counters start there
  let from = 0;
  events.forEach((e, i) => {
    if (e.msg === 'supervisor up') from = i;
  });
  const out = {};
  const row = (n) => (out[n] ??= { requested: 0, startFailures: 0, crashes: 0, lastCrash: null, lastStartFailure: null });
  const asked = new Map();
  let stopping = false;
  for (const e of events.slice(from)) {
    const t = Date.parse(e.t);
    if (e.msg === 'stopping') stopping = true;
    if (e.msg === 'restart requested' || e.msg === 'hold') asked.set(e.name, t);
    if (e.msg !== 'exited' || stopping) continue;
    const r = row(e.name);
    const at = asked.get(e.name);
    // the supervisor marks the exits it caused itself (`requested: true`); an older supervisor only logged the request before the exit
    if (e.requested || (at !== undefined && t - at <= REQUEST_WINDOW_MS)) {
      r.requested += 1;
      asked.delete(e.name);
    } else if ((e.upSec ?? 0) < START_FAIL_SEC) {
      r.startFailures += 1;
      r.lastStartFailure = e.t;
    } else {
      r.crashes += 1;
      r.lastCrash = { at: e.t, upSec: e.upSec };
    }
  }
  return out;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const here = dirname(fileURLToPath(import.meta.url));
  const log = process.argv[2] ?? join(here, '..', 'run', 'logs', 'supervisor.log');
  process.stdout.write(JSON.stringify(classifyExits(readFileSync(log, 'utf8').split('\n'))) + '\n');
}
