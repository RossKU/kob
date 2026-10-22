#!/usr/bin/env node
// Post-deploy check of a hosted web app (.github/workflows/deploy-pages.yml, docs/ops/web-hosting.md): the host must send what
// public/_headers asks for, and serve exactly the built files.
//
//   node scripts/check-pages.mjs <base url> [--dist dist]
//
// * `/`: Content-Security-Policy equal to CSP_HEADER of csp.mjs (with `frame-ancestors 'none'`), X-Frame-Options DENY,
//   X-Content-Type-Options nosniff, Referrer-Policy no-referrer.
// * `/signer.html` and `/signer` (Cloudflare Pages serves the page under both): the signer's own policy (SIGNER_CSP_HEADER:
//   sandbox, `frame-ancestors 'self'`) and X-Frame-Options SAMEORIGIN, never the app's `frame-ancestors 'none'` / DENY (the app
//   could not frame its signer).
// * a file under `/assets/`: Access-Control-Allow-Origin `*` (the sandboxed signer loads its bundle cross-origin).
// * with --dist: every built file (except the host's own `_headers` / `_redirects`) is fetched and its sha256 compared with the
//   local file, so a host that rewrites pages (an injected analytics script, minification, an old deployment) is caught.
// Redirects are followed (Pages sends `/index.html` to `/`). Prints one line per check; exits 1 when any fails.
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CSP_HEADER, SIGNER_CSP_HEADER } from '../csp.mjs';

const HOST_FILES = new Set(['_headers', '_redirects']);
const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

/** Every file under `dir`, as URL paths (`/assets/x.js`), sorted. */
export function distFiles(dir) {
  const out = [];
  const walk = (d) => {
    for (const name of readdirSync(d)) {
      const p = join(d, name);
      if (statSync(p).isDirectory()) walk(p);
      else out.push('/' + relative(dir, p).split(sep).join('/'));
    }
  };
  walk(dir);
  return out.sort();
}

/**
 * Runs the checks against `base`. `fetchFn` is injectable (tests); `attempts` / `delayMs` cover a deployment that is not reachable yet
 * (only the first request is retried: a wrong header on a reachable deployment is a failure, not a delay).
 * @returns {Promise<{ ok: boolean, lines: string[] }>}
 */
export async function checkPages(base, { dist, fetchFn = fetch, attempts = 6, delayMs = 5000 } = {}) {
  const root = new URL(base.endsWith('/') ? base : base + '/');
  const lines = [];
  let ok = true;
  const pass = (m) => lines.push(`ok    ${m}`);
  const fail = (m) => {
    ok = false;
    lines.push(`FAIL  ${m}`);
  };
  const get = (path) => fetchFn(new URL(path.replace(/^\//, ''), root), { redirect: 'follow', cache: 'no-store' });

  let index;
  for (let i = 1; ; i++) {
    try {
      index = await get('/');
      if (index.status === 200 || i >= attempts) break;
    } catch (e) {
      if (i >= attempts) {
        fail(`/ unreachable: ${e instanceof Error ? e.message : String(e)}`);
        return { ok, lines };
      }
    }
    await new Promise((r) => setTimeout(r, delayMs));
  }
  const expect = (path, res, name, want) => {
    const got = res.headers.get(name);
    if (got === want) pass(`${path} ${name}: ${want.length > 60 ? want.slice(0, 57) + '...' : want}`);
    else fail(`${path} ${name}: expected \`${want}\`, got ${got === null ? 'none' : `\`${got}\``}`);
  };

  if (index.status !== 200) fail(`/ answered HTTP ${index.status}`);
  else {
    expect('/', index, 'content-security-policy', CSP_HEADER);
    expect('/', index, 'x-frame-options', 'DENY');
    expect('/', index, 'x-content-type-options', 'nosniff');
    expect('/', index, 'referrer-policy', 'no-referrer');
  }

  for (const path of ['/signer.html', '/signer']) {
    const res = await get(path);
    if (res.status !== 200) {
      fail(`${path} answered HTTP ${res.status}`);
      continue;
    }
    const csp = res.headers.get('content-security-policy') ?? '';
    const xfo = res.headers.get('x-frame-options') ?? '';
    if (csp.includes("frame-ancestors 'none'") || /deny/i.test(xfo)) fail(`${path} carries the app's frame-ancestors 'none' / X-Frame-Options DENY: the app cannot frame its signer`);
    expect(path, res, 'content-security-policy', SIGNER_CSP_HEADER);
    expect(path, res, 'x-frame-options', 'SAMEORIGIN');
  }

  const files = dist ? distFiles(resolve(dist)).filter((f) => !HOST_FILES.has(f.slice(1))) : [];
  const asset = files.find((f) => f.startsWith('/assets/') && f.endsWith('.js')) ?? (await firstScript(index));
  if (!asset) fail('no file under /assets/ to check Access-Control-Allow-Origin on');
  else {
    const res = await get(asset);
    if (res.status !== 200) fail(`${asset} answered HTTP ${res.status}`);
    else expect(asset, res, 'access-control-allow-origin', '*');
  }

  if (dist) {
    let same = 0;
    for (const f of files) {
      const res = await get(f);
      const body = Buffer.from(await res.arrayBuffer());
      const want = sha256(readFileSync(join(resolve(dist), ...f.slice(1).split('/'))));
      if (res.status !== 200) fail(`${f} answered HTTP ${res.status}`);
      else if (sha256(body) !== want) fail(`${f} served bytes differ from the build (sha256 ${sha256(body)}, built ${want})`);
      else same++;
    }
    if (same === files.length) pass(`all ${files.length} built files are served byte for byte`);
  }
  return { ok, lines };
}

/** The first `/assets/` script that index.html references (when no dist is given). */
async function firstScript(res) {
  try {
    const html = await res.clone().text();
    return html.match(/src="\.?(\/assets\/[^"]+\.js)"/)?.[1] ?? null;
  } catch {
    return null;
  }
}

async function main(argv) {
  const base = argv.find((a) => !a.startsWith('--') && argv[argv.indexOf(a) - 1] !== '--dist');
  const i = argv.indexOf('--dist');
  if (!base || !/^https?:\/\//.test(base)) {
    console.error('usage: node scripts/check-pages.mjs <base url> [--dist dist]');
    return 2;
  }
  const { ok, lines } = await checkPages(base, { dist: i >= 0 ? argv[i + 1] : undefined });
  for (const l of lines) console.log(l);
  console.log(ok ? `${base}: every check passed` : `${base}: CHECK FAILED`);
  return ok ? 0 : 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exit(await main(process.argv.slice(2)));
