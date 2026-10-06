// Locate the test executables of the kob-tests integration-test binaries.
import fs from 'node:fs';
import path from 'node:path';
import cp from 'node:child_process';

/** Newest `<bin>-<hash>[.exe]` in `dir`. */
export function findInDir(dir, bin) {
  const esc = bin.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(`^${esc}-[0-9a-f]+(\\.exe)?$`);
  const hits = fs
    .readdirSync(dir)
    .filter(f => re.test(f))
    .map(f => ({ p: path.join(dir, f), t: fs.statSync(path.join(dir, f)).mtimeMs }))
    .sort((a, b) => b.t - a.t);
  if (!hits.length) throw new Error(`no test executable ${bin}-<hash>[.exe] in ${dir}`);
  return hits[0].p;
}

/** `cargo test -p kob-tests --test <bin> --no-run --message-format=json`; returns the executable of the test target. */
export function findViaCargo(root, bin, { release = false } = {}) {
  const args = ['test', '-p', 'kob-tests', '--test', bin, '--no-run', '--message-format=json'];
  if (release) args.push('--release');
  const r = cp.spawnSync('cargo', args, { cwd: root, encoding: 'utf8', maxBuffer: 1 << 28, stdio: ['ignore', 'pipe', 'inherit'] });
  if (r.status !== 0) throw new Error(`cargo ${args.join(' ')} failed (exit ${r.status})`);
  let exe = null;
  for (const line of r.stdout.split('\n')) {
    if (!line.startsWith('{')) continue;
    let j;
    try {
      j = JSON.parse(line);
    } catch {
      continue;
    }
    if (j.reason === 'compiler-artifact' && j.executable && j.target?.name === bin && j.target.kind?.includes('test')) exe = j.executable;
  }
  if (!exe) throw new Error(`cargo did not report an executable for test target ${bin}`);
  return exe;
}
