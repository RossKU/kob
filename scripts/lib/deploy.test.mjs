// The lock-history check of deploy.mjs against a real git history (audit 2026-11-24 R4F-1).
//
//   node --test scripts/lib/deploy.test.mjs
//
// Each case builds a temporary repository with the inputs deploy.mjs reads (the artifacts, the
// silverc lock and the committed record) and runs the script in it.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { after, test } from 'node:test';

const root = fileURLToPath(new URL('../../', import.meta.url));
const script = join(root, 'scripts/lib/deploy.mjs');
const NET = 'testnet-10';
const RECORD = `contracts/deploy/${NET}/deployment.json`;
const dirs = [];
after(() => { for (const d of dirs) rmSync(d, { recursive: true, force: true }); });

const git = (cwd, ...args) => {
  const r = spawnSync('git', args, { cwd, encoding: 'utf8' });
  assert.equal(r.status, 0, `git ${args.join(' ')}: ${r.stderr}`);
  return r.stdout;
};
const run = (cwd, env = {}) => {
  const e = { ...process.env, ...env };
  if (!('KOB_DEPLOY_NO_HISTORY' in env)) delete e.KOB_DEPLOY_NO_HISTORY;
  return spawnSync(process.execPath, [script, NET, 'out'], { cwd, encoding: 'utf8', env: e });
};

// The current lock template, and an earlier one kept under its own artifact (a re-pin).
const cur = JSON.parse(readFileSync(join(root, RECORD), 'utf8')).locks.find((l) => l.name === 'KobLockAuto');
const oldHash = 'ab'.repeat(32);
const old = { ...cur, artifact: 'contracts/artifacts/KobLockAutoV0.json', hash: oldHash };

function repo() {
  const d = mkdtempSync(join(tmpdir(), 'kob-deploy-'));
  dirs.push(d);
  cpSync(join(root, 'contracts/artifacts'), join(d, 'contracts/artifacts'), { recursive: true });
  cpSync(join(root, 'contracts/silverc.lock'), join(d, 'contracts/silverc.lock'));
  const a = JSON.parse(readFileSync(join(root, 'contracts/artifacts/KobLockAuto.json'), 'utf8'));
  a.contracts.KobLockAuto.compiled.template_hash = [...Buffer.from(oldHash, 'hex')];
  writeFileSync(join(d, old.artifact), JSON.stringify(a));
  mkdirSync(join(d, `contracts/deploy/${NET}`), { recursive: true });
  git(d, 'init', '-q');
  git(d, 'config', 'user.name', 'test');
  git(d, 'config', 'user.email', 'test@example.invalid');
  git(d, 'config', 'commit.gpgsign', 'false');
  git(d, 'config', 'core.autocrlf', 'false');
  return d;
}
const commit = (d, locks, msg, extra = {}) => {
  mkdirSync(join(d, `contracts/deploy/${NET}`), { recursive: true });
  writeFileSync(join(d, RECORD), `${JSON.stringify({ network: NET, ...extra, locks }, null, 2)}\n`);
  git(d, 'add', '-A');
  git(d, 'commit', '-q', '-m', msg);
};

test('two commits of the record, then a third that drops an issued lock: fails', () => {
  const d = repo();
  commit(d, [cur, old], 'first');
  commit(d, [cur, old], 'second', { note: 'a later edit' });
  commit(d, [cur], 'drop the old lock');
  const r = run(d);
  assert.equal(r.status, 1, r.stderr);
  assert.match(r.stderr, new RegExp(`lock template KobLockAuto ${oldHash} was issued`));
});

test('the same history without the deletion passes, also across a commit that deleted the record', () => {
  const d = repo();
  commit(d, [cur, old], 'first');
  commit(d, [cur, old], 'second', { note: 'a later edit' });
  git(d, 'rm', '-q', RECORD);
  git(d, 'commit', '-q', '-m', 'record deleted');
  commit(d, [cur, old], 'record back');
  const r = run(d);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(JSON.parse(readFileSync(join(d, 'out/deployment.json'), 'utf8')).locks.map((l) => l.hash), [cur.hash, oldHash]);
});

test('a shallow clone fails; KOB_DEPLOY_NO_HISTORY=1 skips the check and says so', () => {
  const d = repo();
  commit(d, [cur, old], 'first');
  commit(d, [cur], 'drop the old lock');
  const s = mkdtempSync(join(tmpdir(), 'kob-deploy-shallow-'));
  dirs.push(s);
  git(s, 'clone', '-q', '--depth', '1', `file://${d.replace(/\\/g, '/')}`, 'c');
  const c = join(s, 'c');
  const r = run(c);
  assert.equal(r.status, 1, r.stderr);
  assert.match(r.stderr, /this clone is shallow/);
  const skip = run(c, { KOB_DEPLOY_NO_HISTORY: '1' });
  assert.equal(skip.status, 0, skip.stderr);
  assert.match(skip.stderr, /KOB_DEPLOY_NO_HISTORY=1: the lock-history check of .* is skipped/);
});
