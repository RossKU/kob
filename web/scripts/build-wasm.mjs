// Builds kob-wasm (scripts/build-wasm.sh --web --slim at the repo root; needs the wasm32 target and wasm-bindgen-cli 0.2.100,
// see that script's header) and copies the bindings to web/wasm/{web,node} (gitignored). The slim variant has no x402
// payer / merchant exports: the browser bundle carries no entry point that accepts a secret key.
//   npm run build:wasm            build + copy
//   npm run build:wasm -- --copy  copy only (bindings already built)
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const WEB = join(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = join(WEB, '..');
if (!process.argv.includes('--copy')) {
  const r = spawnSync('bash', ['scripts/build-wasm.sh', '--web', '--slim'], { cwd: REPO, stdio: 'inherit' });
  if (r.status !== 0) process.exit(r.status ?? 1);
}
for (const [from, to] of [['pkg-slim', 'web'], ['pkg-node-slim', 'node']]) {
  const src = join(REPO, 'crates', 'kob-wasm', from);
  if (!existsSync(src)) throw new Error(`missing ${src}: run without --copy`);
  const dst = join(WEB, 'wasm', to);
  rmSync(dst, { recursive: true, force: true });
  mkdirSync(dst, { recursive: true });
  cpSync(src, dst, { recursive: true });
}
// the node bindings are CommonJS; web/package.json says module
writeFileSync(join(WEB, 'wasm', 'node', 'package.json'), '{"type":"commonjs"}\n');
console.log('kob-wasm bindings copied to web/wasm/{web,node}');
