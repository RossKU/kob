// Restart causes of the supervisor's children (scripts/restarts.mjs): requested / held, start failures of a deploy step, crashes.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { classifyExits } from '../scripts/restarts.mjs';

const L = (t: string, msg: string, f: Record<string, unknown> = {}) => JSON.stringify({ t: `2026-10-10T${t}Z`, src: 'supervisor', msg, ...f });

test('only the latest supervisor run counts; an exit right after a restart request is requested, a quick one a start failure, else a crash', () => {
  const lines = [
    L('10:00:00.000', 'supervisor up'),
    L('10:01:00.000', 'exited', { name: 'bots', upSec: 60 }),
    L('11:00:00.000', 'supervisor up'),
    // the 10-10 redeploy: bundle renamed, then the child crash-looped at once
    L('13:37:08.803', 'restart requested', { name: 'bots' }),
    L('13:37:08.959', 'exited', { name: 'bots', upSec: 311 }),
    L('13:37:23.055', 'exited', { name: 'bots', upSec: 0 }),
    L('13:37:27.136', 'exited', { name: 'bots', upSec: 0 }),
    // a crash nobody asked for
    L('15:00:00.000', 'exited', { name: 'exec-a', upSec: 7200 }),
    // the new supervisor marks its own kills, holds included
    L('16:00:00.000', 'hold', { name: 'exec-b' }),
    L('16:00:00.100', 'exited', { name: 'exec-b', upSec: 9000, requested: true }),
    L('16:00:02.200', 'held', { name: 'exec-b', path: 'run/HOLD-exec-b' }),
    L('17:00:00.000', 'stopping', { reason: 'STOP file' }),
    L('17:00:00.100', 'exited', { name: 'exec-a', upSec: 100 }),
  ];
  const r = classifyExits(lines);
  assert.deepEqual(r.bots, { requested: 1, startFailures: 2, crashes: 0, lastCrash: null, lastStartFailure: '2026-10-10T13:37:27.136Z' });
  assert.deepEqual(r['exec-a'], { requested: 0, startFailures: 0, crashes: 1, lastCrash: { at: '2026-10-10T15:00:00.000Z', upSec: 7200 }, lastStartFailure: null });
  assert.equal(r['exec-b']!.requested, 1);
  assert.equal(r['exec-b']!.crashes, 0);
});
