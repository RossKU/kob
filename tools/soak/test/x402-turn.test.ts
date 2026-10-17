// Every payer turn of the x402 bot is visible in the log: its start (the path the pick chose), a warning within a bounded time
// while it runs long, and its end with its duration, whether it returns or throws (TN10 2026-10-17: after the binding 1.0.0
// cutover the log could not tell an unpicked path from a turn that never ended).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loggedTurn } from '../src/bots/x402-turn.ts';
import type { Logger } from '../src/log.ts';

function memLog() {
  const lines: { lvl: string; msg: string; f: Record<string, unknown> }[] = [];
  const at = (lvl: string) => (msg: string, f: Record<string, unknown> = {}) => void lines.push({ lvl, msg, f });
  const log: Logger = { debug: at('debug'), info: at('info'), warn: at('warn'), error: at('error'), child: () => log };
  return { log, lines };
}

test('a turn logs its start and its end, with the path and the duration', async () => {
  const { log, lines } = memLog();
  assert.equal(await loggedTurn(log, '/native', async () => 7), 7);
  assert.deepEqual(lines.map((l) => [l.lvl, l.msg, l.f.path]), [
    ['info', 'x402 turn', '/native'],
    ['info', 'x402 turn ended', '/native'],
  ]);
  assert.equal(typeof lines[1]!.f.ms, 'number');
});

test('a turn that throws still ends in a log line, and the error reaches the caller', async () => {
  const { log, lines } = memLog();
  await assert.rejects(loggedTurn(log, '/swap', async () => { throw new Error('boom'); }), /boom/);
  assert.deepEqual(lines.map((l) => [l.lvl, l.msg, l.f.threw]), [
    ['info', 'x402 turn', undefined],
    ['warn', 'x402 turn ended', 'boom'],
  ]);
});

test('a turn that never ends is reported within a bounded time, repeatedly, and is not abandoned', async () => {
  const { log, lines } = memLog();
  let release!: () => void;
  const stuck = new Promise<void>((r) => (release = r));
  const turn = loggedTurn(log, '/retry/drop', () => stuck, { warnAfterMs: 20 });
  await new Promise((r) => setTimeout(r, 75));
  const warns = lines.filter((l) => l.msg === 'x402 turn still running');
  assert.ok(warns.length >= 2, `warned ${warns.length} time(s) within 75 ms`);
  assert.ok(warns.every((w) => w.f.path === '/retry/drop' && typeof w.f.ms === 'number'));
  assert.ok(!lines.some((l) => l.msg === 'x402 turn ended'), 'still running: not abandoned');
  release();
  await turn;
  const n = lines.filter((l) => l.msg === 'x402 turn still running').length;
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(lines.filter((l) => l.msg === 'x402 turn still running').length, n, 'no warning after the end');
  assert.equal(lines.at(-1)!.msg, 'x402 turn ended');
});
