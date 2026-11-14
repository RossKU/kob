// Daily, KEYLESS drift check of the wallet extensions the real-wallet runs are pinned to (lib/extension-pins.mjs).
//
//   node tools/wallet-gate/scripts/check-latest.mjs [--json] [--out <file>] [--write-baseline]
//
// Downloads the newest KasWare CRX (the Chrome Web Store endpoint always serves the newest) and looks up the newest Kaspire extension
// release on GitHub (GITHUB_TOKEN is used when set), then compares version and sha256 with the pins. On a difference it prints a markdown
// report: the new version, a diff of the provider method names and a diff of refusal-like strings in the background script / service
// worker, old pinned build against new build. A text diff is a prompt to look, not proof.
//
// Exit code: 0 everything equals the pins, 1 a wallet drifted, 2 a download / parse error.
// KasWare's store serves only the newest build, so the pinned build's method list and strings are kept in wallet-baseline.json (next to
// this script; regenerate it with --write-baseline after moving a pin). Kaspire's pinned zip is downloaded for the diff.
// Everything downloaded is untrusted data: it is hashed and read as text, never executed, imported or written to disk. Node built-ins only.
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateRawSync } from 'node:zlib';
import { EXTENSIONS, crxToZip, packageVersion, sha256Hex } from '../lib/extension-pins.mjs';

const BASELINE_PATH = join(dirname(fileURLToPath(import.meta.url)), 'wallet-baseline.json');
const KASPIRE_RELEASES = 'https://api.github.com/repos/KaspaHUB21/Kaspire-Kaspa-Wallet/releases?per_page=30';
const ZIP_NAME = /^kaspire-extension-(\d+\.\d+\.\d+)\.zip$/;
const REFUSAL = /not an unspent|could not be verified|unreachable|already spent|refus|reject|invalid|not allowed/i;
const MAX_STRINGS = 1000; // per wallet, kept in the baseline and the diff
const MAX_STRING_LEN = 160;
const MAX_LIST_IN_REPORT = 60;

// ------------------------------------------------------------------------------------------------ zip (central directory)

/** Reads a zip into `{ name: Buffer }` (stored and deflated entries). Throws on anything it does not understand. */
function unzip(buf) {
  const b = Buffer.from(buf);
  let eocd = -1;
  for (let i = b.length - 22; i >= Math.max(0, b.length - 22 - 65535); i--) {
    if (b.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error('zip: end of central directory not found');
  const count = b.readUInt16LE(eocd + 10);
  let p = b.readUInt32LE(eocd + 16);
  const files = {};
  for (let n = 0; n < count; n++) {
    if (b.readUInt32LE(p) !== 0x02014b50) throw new Error('zip: bad central directory entry');
    const method = b.readUInt16LE(p + 10);
    const csize = b.readUInt32LE(p + 20);
    const nameLen = b.readUInt16LE(p + 28);
    const extraLen = b.readUInt16LE(p + 30);
    const commentLen = b.readUInt16LE(p + 32);
    const local = b.readUInt32LE(p + 42);
    const name = b.toString('utf8', p + 46, p + 46 + nameLen);
    p += 46 + nameLen + extraLen + commentLen;
    if (name.endsWith('/')) continue;
    if (b.readUInt32LE(local) !== 0x04034b50) throw new Error(`zip: bad local header for ${name}`);
    const start = local + 30 + b.readUInt16LE(local + 26) + b.readUInt16LE(local + 28);
    const raw = b.subarray(start, start + csize);
    if (method === 0) files[name] = Buffer.from(raw);
    else if (method === 8) files[name] = inflateRawSync(raw);
    else throw new Error(`zip: unsupported compression method ${method} for ${name}`);
  }
  return files;
}

// ------------------------------------------------------------------------------------------------ extraction (text only)

const text = (files, name) => (files[name] ? files[name].toString('utf8') : '');

/** Distinct string literals of `src` that look like a refusal / error message, sorted, length- and count-capped. */
function refusalStrings(src) {
  const out = new Set();
  for (const m of src.matchAll(/"((?:[^"\\\n]|\\.){8,400})"|'((?:[^'\\\n]|\\.){8,400})'|`((?:[^`\\]|\\.){8,400})`/g)) {
    const s = (m[1] ?? m[2] ?? m[3]).replace(/\$\{[^}]*\}/g, '#').replace(/\s+/g, ' ').trim(); // a template placeholder becomes #
    // a real message starts with a letter, has a space and no code punctuation (a mis-paired quote in minified code yields fragments)
    if (REFUSAL.test(s) && /^[A-Za-z]/.test(s) && /\s/.test(s) && !/[{};]|=>/.test(s)) out.add(s.length > MAX_STRING_LEN ? s.slice(0, MAX_STRING_LEN) + '...' : s);
  }
  return [...out].sort().slice(0, MAX_STRINGS);
}

const JS_KEYWORDS = new Set(['if', 'for', 'while', 'switch', 'catch', 'function', 'return', 'constructor']);

/** Method names of the injected provider: `this.x = async (...)` style (KasWare) or class methods `[async] x(...) {` (Kaspire). */
function methodNames(src) {
  const out = new Set();
  for (const m of src.matchAll(/this\.([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?(?:\(|function|[A-Za-z_$][\w$]*\s*=>)/g)) out.add(m[1]);
  for (const m of src.matchAll(/^[ \t]+(?:async\s+)?([A-Za-z_$][\w$]*)\s*\([^)\n]*\)\s*\{/gm)) if (!JS_KEYWORDS.has(m[1])) out.add(m[1]);
  return [...out].sort();
}

function describe(wallet, files, sha256) {
  const version = packageVersion(files);
  if (!version) throw new Error(`${wallet}: no manifest version in the package`);
  // the version ends up in an issue title: only a plain dotted version is accepted from a download
  if (!/^\d{1,5}(\.\d{1,5}){0,3}$/.test(String(version))) throw new Error(`${wallet}: unexpected manifest version ${JSON.stringify(String(version).slice(0, 40))}`);
  if (wallet === 'kasware') {
    return { version, sha256, methods: methodNames(text(files, 'extensionPageScript.js')), strings: refusalStrings(text(files, 'extensionServiceWorker.js')) };
  }
  return { version, sha256, methods: methodNames(text(files, 'inpage.js')), strings: refusalStrings(text(files, 'background.js')) };
}

// ------------------------------------------------------------------------------------------------ downloads

async function get(url, headers = {}) {
  const res = await fetch(url, { redirect: 'follow', headers: { 'user-agent': 'wallet-drift-check', ...headers } });
  if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`);
  return Buffer.from(await res.arrayBuffer());
}

async function fetchKasware() {
  const pkg = await get(EXTENSIONS.kasware.crxUrl);
  return describe('kasware', unzip(crxToZip(pkg)), sha256Hex(pkg));
}

async function fetchKaspireZip(url) {
  const pkg = await get(url);
  return describe('kaspire', unzip(pkg), sha256Hex(pkg));
}

/** The newest Kaspire release that carries an extension zip (the newest release overall is often an Android build only). */
async function newestKaspireAsset() {
  const headers = { accept: 'application/vnd.github+json', ...(process.env.GITHUB_TOKEN ? { authorization: `Bearer ${process.env.GITHUB_TOKEN}` } : {}) };
  const releases = JSON.parse((await get(KASPIRE_RELEASES, headers)).toString('utf8'));
  let best = null;
  for (const r of releases) {
    for (const a of r.assets ?? []) {
      const m = ZIP_NAME.exec(a.name);
      if (!m) continue;
      const key = m[1].split('.').map(Number);
      if (!best || cmp(key, best.key) > 0) best = { key, version: m[1], url: a.browser_download_url, tag: r.tag_name };
    }
  }
  if (!best) throw new Error('no kaspire-extension-*.zip asset among the newest 30 releases');
  return best;
}
const cmp = (a, b) => a.reduce((d, n, i) => d || n - b[i], 0);

// ------------------------------------------------------------------------------------------------ report

const listDiff = (oldL, newL) => {
  const o = new Set(oldL), n = new Set(newL);
  return { added: newL.filter((x) => !o.has(x)), removed: oldL.filter((x) => !n.has(x)) };
};

function section(title, d) {
  if (!d.added.length && !d.removed.length) return `${title}: unchanged.\n`;
  const fmt = (sign, list) => list.slice(0, MAX_LIST_IN_REPORT).map((x) => `${sign} ${x}`).join('\n') + (list.length > MAX_LIST_IN_REPORT ? `\n${sign} ... ${list.length - MAX_LIST_IN_REPORT} more` : '');
  return `${title}:\n\n\`\`\`diff\n${[fmt('+', d.added), fmt('-', d.removed)].filter(Boolean).join('\n')}\n\`\`\`\n`;
}

function walletEntry(wallet, pinned, latest, old) {
  const spec = EXTENSIONS[wallet];
  const sha = spec.zipUrl ? spec.zipSha256 : spec.crxSha256;
  const drift = latest.version !== spec.version || latest.sha256 !== sha;
  return {
    wallet, label: spec.label, drift, pinnedVersion: spec.version, pinnedSha256: sha, latestVersion: latest.version, latestSha256: latest.sha256,
    methods: old ? listDiff(old.methods, latest.methods) : null, strings: old ? listDiff(old.strings, latest.strings) : null, hasOld: !!old,
  };
}

function markdown(entries) {
  const drifted = entries.filter((e) => e.drift);
  const title = drifted.map((e) => `${e.label} ${e.latestVersion} (KOB tested with ${e.pinnedVersion})`).join(', ');
  const lines = [`# Wallet release drift: ${title}`, ''];
  for (const e of drifted) {
    lines.push(`## ${e.label} ${e.latestVersion} released, KOB tested with ${e.pinnedVersion}`, '');
    if (e.latestVersion === e.pinnedVersion) lines.push(`Same version, different build: sha256 ${e.latestSha256} (pinned ${e.pinnedSha256}).`, '');
    else lines.push(`Newest sha256 ${e.latestSha256} (pinned ${e.pinnedSha256}).`, '');
    if (!e.hasOld) lines.push('No old build to diff against (no baseline).', '');
    else {
      lines.push(section('Provider methods', e.methods), section('Refusal-like strings in the background script / service worker', e.strings));
    }
  }
  lines.push(
    'What to do: read the release notes, run the real-wallet gate on the founder machine (`web/e2e-real`, `--ext latest` fetches the newest build without touching the pins),',
    'then move the pin in `tools/wallet-gate/lib/extension-pins.mjs` and regenerate the baseline (`node tools/wallet-gate/scripts/check-latest.mjs --write-baseline`).',
    'A text diff is a prompt to look, not proof.',
    '',
  );
  return lines.join('\n');
}

// ------------------------------------------------------------------------------------------------ main

async function main() {
  const argv = process.argv.slice(2);
  const outIdx = argv.indexOf('--out');
  const out = outIdx >= 0 ? argv[outIdx + 1] : null;
  if (outIdx >= 0 && (!out || out.startsWith('--'))) throw new Error('--out needs a file');

  if (argv.includes('--write-baseline')) {
    const base = {};
    const kw = await fetchKasware();
    if (kw.version !== EXTENSIONS.kasware.version || kw.sha256 !== EXTENSIONS.kasware.crxSha256) throw new Error(`the store serves KasWare ${kw.version} (${kw.sha256}), not the pin ${EXTENSIONS.kasware.version}: the baseline is made from the pinned build`);
    base.kasware = kw;
    const kp = await fetchKaspireZip(EXTENSIONS.kaspire.zipUrl);
    if (kp.version !== EXTENSIONS.kaspire.version || kp.sha256 !== EXTENSIONS.kaspire.zipSha256) throw new Error(`the pinned Kaspire zip is ${kp.version} (${kp.sha256}), not the pin ${EXTENSIONS.kaspire.version}`);
    base.kaspire = kp;
    writeFileSync(BASELINE_PATH, JSON.stringify(base, null, 1) + '\n');
    console.log(`wrote ${BASELINE_PATH}: KasWare ${kw.version} (${kw.methods.length} methods, ${kw.strings.length} strings), Kaspire ${kp.version} (${kp.methods.length} methods, ${kp.strings.length} strings)`);
    return 0;
  }

  let baseline = {};
  try {
    baseline = JSON.parse(readFileSync(BASELINE_PATH, 'utf8'));
  } catch {
    /* no baseline: the report says so */
  }
  const entries = [];

  const kw = await fetchKasware();
  const kwOld = baseline.kasware?.version === EXTENSIONS.kasware.version && baseline.kasware?.sha256 === EXTENSIONS.kasware.crxSha256 ? baseline.kasware : null;
  entries.push(walletEntry('kasware', EXTENSIONS.kasware, kw, kwOld));

  const asset = await newestKaspireAsset();
  const kp = await fetchKaspireZip(asset.url);
  let kpOld = null;
  if (kp.version !== EXTENSIONS.kaspire.version || kp.sha256 !== EXTENSIONS.kaspire.zipSha256) {
    try {
      const o = await fetchKaspireZip(EXTENSIONS.kaspire.zipUrl);
      if (o.sha256 === EXTENSIONS.kaspire.zipSha256) kpOld = o;
    } catch {
      /* fall back to the baseline */
    }
    kpOld ??= baseline.kaspire?.sha256 === EXTENSIONS.kaspire.zipSha256 ? baseline.kaspire : null;
  }
  entries.push(walletEntry('kaspire', EXTENSIONS.kaspire, kp, kpOld));

  const drifted = entries.some((e) => e.drift);
  const md = drifted ? markdown(entries) : '';
  if (argv.includes('--json')) console.log(JSON.stringify({ drift: drifted, wallets: entries }, null, 2));
  else if (drifted) console.log(md);
  else for (const e of entries) console.log(`${e.label} ${e.latestVersion} equals the pin (sha256 ${e.latestSha256})`);
  if (out && drifted) writeFileSync(out, md);
  return drifted ? 1 : 0;
}

main().then(
  (code) => process.exit(code),
  (e) => {
    console.error('check-latest failed:', e?.message ?? e);
    process.exit(2);
  },
);
