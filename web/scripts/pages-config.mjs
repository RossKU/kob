#!/usr/bin/env node
// The runtime configuration of a public web deployment (.github/workflows/deploy-pages.yml, docs/ops/web-hosting.md): writes
// <dist>/config.json from the KOB_PAGES_* repository variables and, on testnet-10, <dist>/registry/tokens.json as the supported
// token programs with no token entries (the shipped registry names mainnet tokens; the same file `kob-executor setup` writes as
// kob-tokens.json for a testnet-10 build).
//
//   node scripts/pages-config.mjs --check          validate the variables only (nothing is written)
//   node scripts/pages-config.mjs --dist dist      validate, then write the files into the built app (after `npm run build:release`)
//
// Variables (environment):
//   KOB_PAGES_NETWORK             testnet-10 (default) or mainnet
//   KOB_PAGES_INDEXER_URL         required: the indexer the app reads (https, a public host)
//   KOB_PAGES_EXTRA_INDEXER_URLS  optional: further indexers: the app cross-checks against them and reads from one when the primary is down (comma-separated, https, at most 4)
//   KOB_PAGES_NODE_URL            optional: a wRPC node (wss) WITH its path, e.g. wss://<host>/kaspa/testnet-10/wrpc/json (a bare host answers 404 at the
//                                 handshake; a note says so); empty = a public node the Kaspa SDK's resolver names (reached over Borsh)
//   KOB_PAGES_EXPLORER_URL        optional: block explorer base (https); empty = kaspa.stream of the network
//   KOB_PAGES_TOKEN_NAMES_URL     optional: the base of the token names and icons the market list reads, `<base>/<network>/tokens.json` in the shape of
//                                 kascov.io's data set (https); empty or unset = https://kascov.io/data (the app's default); `off` = no token names
//                                 (config.json `tokenNamesUrl: ""`; GitHub cannot tell an empty variable from an unset one, hence the word)
//
// A public page is served over https, so every URL must be https / wss (a browser blocks http / ws from it anyway), name a public
// host (not localhost or a private address) and carry no user name, password, query or fragment (config.json is public: an API key
// in it would be published). Prints the config written; exits 1 with every problem listed.
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DEFAULT_REGISTRY_PATH } from '../registry-pin.mjs';

export const NETWORKS = ['testnet-10', 'mainnet'];
export const MAX_EXTRA_INDEXERS = 4;

/** True for a host name that is not reachable from the public internet (loopback, private, link-local, unspecified). */
export function isPrivateHost(hostname) {
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, '');
  if (h === 'localhost' || h.endsWith('.localhost') || h.endsWith('.local') || h.endsWith('.internal')) return true;
  const v4 = h.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/);
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])];
    return a === 0 || a === 10 || a === 127 || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b <= 127);
  }
  if (h.includes(':')) return h === '::' || h === '::1' || /^f[cd]/.test(h) || /^fe[89ab]/.test(h) || h.startsWith('::ffff:');
  return false;
}

/** A public URL of one of `schemes`, trailing slashes removed; or an error message. */
export function publicUrl(name, value, schemes) {
  const v = String(value ?? '').trim();
  let u;
  try {
    u = new URL(v);
  } catch {
    return { error: `${name}: \`${v}\` is not a URL` };
  }
  if (!schemes.includes(u.protocol)) return { error: `${name}: \`${v}\` must use ${schemes.map((s) => s.replace(':', '')).join(' or ')} (a public page is https)` };
  if (!u.hostname) return { error: `${name}: \`${v}\` has no host` };
  if (u.username || u.password) return { error: `${name}: \`${v}\` carries a user name or password (config.json is public)` };
  if (u.search || u.hash || v.includes('?') || v.includes('#')) return { error: `${name}: \`${v}\` carries a query or fragment (config.json is public: no API keys in it)` };
  if (isPrivateHost(u.hostname)) return { error: `${name}: \`${v}\` is not a public host (localhost or a private address: visitors cannot reach it)` };
  return { url: v.replace(/\/+$/, '') };
}

/** The form two spellings of one indexer share (web/src/config.ts `indexerKey`). */
const indexerKey = (url) => {
  const u = new URL(url);
  return `${u.protocol}//${u.host}${u.pathname.replace(/\/+$/, '')}`;
};

/**
 * The deployment's config.json from the KOB_PAGES_* variables: `{ config, errors, notes }`. `config` is null when anything is wrong.
 * @param {Record<string, string | undefined>} env
 */
export function pagesConfig(env) {
  const errors = [];
  const notes = [];
  const network = (env.KOB_PAGES_NETWORK ?? '').trim() || 'testnet-10';
  if (!NETWORKS.includes(network)) errors.push(`KOB_PAGES_NETWORK: \`${network}\` must be testnet-10 or mainnet`);

  let indexerUrl = '';
  if (!(env.KOB_PAGES_INDEXER_URL ?? '').trim()) errors.push('KOB_PAGES_INDEXER_URL is empty: the app needs an indexer for the book, the charts and the orders');
  else {
    const r = publicUrl('KOB_PAGES_INDEXER_URL', env.KOB_PAGES_INDEXER_URL, ['https:']);
    if (r.error) errors.push(r.error);
    else indexerUrl = r.url;
  }

  const extraIndexerUrls = [];
  const seen = new Set(indexerUrl ? [indexerKey(indexerUrl)] : []);
  const extras = (env.KOB_PAGES_EXTRA_INDEXER_URLS ?? '').split(/[\s,]+/).filter(Boolean);
  for (const e of extras) {
    const r = publicUrl('KOB_PAGES_EXTRA_INDEXER_URLS', e, ['https:']);
    if (r.error) errors.push(r.error);
    else if (seen.has(indexerKey(r.url))) errors.push(`KOB_PAGES_EXTRA_INDEXER_URLS: \`${r.url}\` is already listed (or is the primary indexer): a cross-check needs another indexer`);
    else {
      seen.add(indexerKey(r.url));
      extraIndexerUrls.push(r.url);
    }
  }
  if (extraIndexerUrls.length > MAX_EXTRA_INDEXERS) errors.push(`KOB_PAGES_EXTRA_INDEXER_URLS: ${extraIndexerUrls.length} indexers, at most ${MAX_EXTRA_INDEXERS}`);
  if (extras.length === 0) notes.push('no KOB_PAGES_EXTRA_INDEXER_URLS: the app shows "single indexer, not cross-checked"');

  let nodeUrl = '';
  if ((env.KOB_PAGES_NODE_URL ?? '').trim()) {
    const r = publicUrl('KOB_PAGES_NODE_URL', env.KOB_PAGES_NODE_URL, ['wss:']);
    if (r.error) errors.push(r.error);
    else {
      nodeUrl = r.url;
      // the public nodes answer 404 at the WebSocket handshake of a bare host: the endpoint includes its wRPC path
      if (!new URL(nodeUrl).pathname.includes('/wrpc/')) notes.push(`KOB_PAGES_NODE_URL: \`${nodeUrl}\` has no /wrpc/ path; a public node needs the whole endpoint, e.g. wss://<host>/kaspa/${network}/wrpc/json (a bare wss://<host> answers 404 and the page shows "Node: unreachable")`);
    }
  } else notes.push('no KOB_PAGES_NODE_URL: the app connects to a public Kaspa node the SDK resolver names');

  let explorerUrl = '';
  if ((env.KOB_PAGES_EXPLORER_URL ?? '').trim()) {
    const r = publicUrl('KOB_PAGES_EXPLORER_URL', env.KOB_PAGES_EXPLORER_URL, ['https:']);
    if (r.error) errors.push(r.error);
    else explorerUrl = r.url;
  }

  // undefined = the app's default (kascov.io); '' = the feature is off
  let tokenNamesUrl;
  const names = (env.KOB_PAGES_TOKEN_NAMES_URL ?? '').trim();
  if (names.toLowerCase() === 'off') {
    tokenNamesUrl = '';
    notes.push('KOB_PAGES_TOKEN_NAMES_URL=off: the market list shows no token names or icons from a third party');
  } else if (names) {
    const r = publicUrl('KOB_PAGES_TOKEN_NAMES_URL', names, ['https:']);
    if (r.error) errors.push(r.error);
    else tokenNamesUrl = r.url;
  }

  if (errors.length) return { config: null, errors, notes };
  const config = { network, indexerUrl, extraIndexerUrls, nodeUrl };
  if (explorerUrl) config.explorerUrl = explorerUrl;
  if (tokenNamesUrl !== undefined) config.tokenNamesUrl = tokenNamesUrl;
  return { config, errors, notes };
}

/**
 * The token-program list of a network without a pinned registry: the shipped registry's programs (templates) with `network` set and
 * no token entries (kob-executor `setup::program_registry`). The app then lists every token of a supported program and names none.
 */
export function programRegistry(registryText, network) {
  const doc = JSON.parse(registryText);
  if (!doc || typeof doc !== 'object' || !Array.isArray(doc.templates) || doc.templates.length === 0) throw new Error('the registry lists no token program');
  const { $schema: _schema, ...rest } = doc;
  return JSON.stringify({ ...rest, network, tokens: [] }, null, 2) + '\n';
}

/** Writes config.json (and the testnet-10 program list) into `dist`; returns the paths written with their sha256. */
export function writeDeployment(dist, config, registryPath = DEFAULT_REGISTRY_PATH) {
  const out = [];
  const put = (rel, text) => {
    const p = join(dist, rel);
    mkdirSync(join(p, '..'), { recursive: true });
    writeFileSync(p, text);
    out.push({ path: rel, sha256: createHash('sha256').update(text).digest('hex') });
  };
  put('config.json', JSON.stringify(config, null, 2) + '\n');
  // mainnet: `npm run build:release` already shipped the pinned registry (scripts/release-hashes.sh checks it)
  if (config.network !== 'mainnet') put('registry/tokens.json', programRegistry(readFileSync(registryPath, 'utf8'), config.network));
  return out;
}

function main(argv) {
  const check = argv.includes('--check');
  const i = argv.indexOf('--dist');
  const dist = i >= 0 ? argv[i + 1] : undefined;
  if (check === Boolean(dist)) {
    console.error('usage: node scripts/pages-config.mjs --check | --dist <dir>');
    return 2;
  }
  const { config, errors, notes } = pagesConfig(process.env);
  if (!config) {
    for (const e of errors) console.error(`error: ${e}`);
    return 1;
  }
  for (const n of notes) console.log(`note: ${n}`);
  if (dist) {
    const dir = resolve(dist);
    try {
      readFileSync(join(dir, 'index.html'));
    } catch {
      console.error(`${dir}/index.html is missing: build the app first (npm run build:release)`);
      return 1;
    }
    for (const f of writeDeployment(dir, config)) console.log(`wrote ${f.path} (sha256 ${f.sha256})`);
  }
  console.log(JSON.stringify(config, null, 2));
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exit(main(process.argv.slice(2)));
