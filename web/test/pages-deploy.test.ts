// The public deployment's helpers (.github/workflows/deploy-pages.yml): config.json from the KOB_PAGES_* variables (scripts/pages-config.mjs)
// and the post-deploy check (scripts/check-pages.mjs), run against a local host that applies public/_headers the way Cloudflare Pages does.
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { createServer, type Server } from 'node:http';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { pagesConfig, programRegistry, writeDeployment } from '../scripts/pages-config.mjs';
import { checkPages } from '../scripts/check-pages.mjs';
import { DEFAULT_REGISTRY_PATH } from '../registry-pin.mjs';
import { sanitizeLayer, type ConfigWarning } from '../src/config';
import { parseRegistry } from '../src/kob/registry';
import { loadKobNode } from '../src/kob/wasm.node';

const IDX = 'https://idx.example.org';

describe('pages-config: config.json from the repository variables', () => {
  it('defaults to testnet-10 and the public resolver, and the app takes the file without a warning', () => {
    const { config, errors, notes } = pagesConfig({ KOB_PAGES_INDEXER_URL: `${IDX}/`, KOB_PAGES_EXTRA_INDEXER_URLS: ' https://b.example.net, https://c.example.com/kob ' });
    expect(errors).toEqual([]);
    expect(config).toEqual({ network: 'testnet-10', indexerUrl: IDX, extraIndexerUrls: ['https://b.example.net', 'https://c.example.com/kob'], nodeUrl: '' });
    expect(notes.join(' ')).toContain('resolver');
    const warnings: ConfigWarning[] = [];
    const patch = sanitizeLayer('config.json', JSON.parse(JSON.stringify(config)), false, warnings);
    expect(warnings).toEqual([]);
    expect(patch).toMatchObject({ network: 'testnet-10', indexerUrl: IDX, nodeUrl: '', extraIndexerUrls: config!.extraIndexerUrls });
  });

  it('a node URL without its wRPC path gets a note (a bare public host answers 404 at the handshake); with the path none', () => {
    const note = (nodeUrl: string) => pagesConfig({ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_NODE_URL: nodeUrl }).notes.filter((n) => n.startsWith('KOB_PAGES_NODE_URL'));
    expect(note('wss://node.example.org')).toEqual([expect.stringContaining('has no /wrpc/ path; a public node needs the whole endpoint, e.g. wss://<host>/kaspa/testnet-10/wrpc/json')]);
    expect(note('wss://node.example.org/kaspa/testnet-10/wrpc/json')).toEqual([]);
    // an empty value: the resolver note, no error
    const empty = pagesConfig({ KOB_PAGES_INDEXER_URL: IDX });
    expect(empty.errors).toEqual([]);
    expect(empty.notes.join(' ')).toContain('SDK resolver names');
  });

  it('takes mainnet, a second indexer, a wss node and an explorer', () => {
    const { config, errors } = pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_EXTRA_INDEXER_URLS: 'https://b.example.net', KOB_PAGES_NODE_URL: 'wss://node.example.org/kaspa', KOB_PAGES_EXPLORER_URL: 'https://explorer.example.org/' });
    expect(errors).toEqual([]);
    expect(config).toEqual({ network: 'mainnet', indexerUrl: IDX, extraIndexerUrls: ['https://b.example.net'], nodeUrl: 'wss://node.example.org/kaspa', explorerUrl: 'https://explorer.example.org' });
    const warnings: ConfigWarning[] = [];
    sanitizeLayer('config.json', config, false, warnings);
    expect(warnings).toEqual([]);
  });

  it('KOB_PAGES_TOKEN_NAMES_URL: unset or blank is the default of the app (no key), a public https URL is taken, `off` turns the names off (an empty string in config.json)', () => {
    expect(pagesConfig({ KOB_PAGES_INDEXER_URL: IDX }).config).not.toHaveProperty('tokenNamesUrl');
    expect(pagesConfig({ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: '  ' }).config).not.toHaveProperty('tokenNamesUrl');
    const own = pagesConfig({ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: 'https://names.example.org/data/' });
    expect(own.errors).toEqual([]);
    expect(own.config).toMatchObject({ tokenNamesUrl: 'https://names.example.org/data' });
    const off = pagesConfig({ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: 'off' });
    expect(off.config).toMatchObject({ tokenNamesUrl: '' });
    expect(off.notes.join(' ')).toContain('no token names');
    // and the app accepts every one of them
    for (const c of [own.config, off.config]) {
      const warnings: ConfigWarning[] = [];
      sanitizeLayer('config.json', c, false, warnings);
      expect(warnings).toEqual([]);
    }
  });

  it.each([
    [{}, 'KOB_PAGES_INDEXER_URL is empty'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: 'http://names.example.org/data' }, 'must use https'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: 'https://localhost/data' }, 'not a public host'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: 'https://names.example.org/data?key=1' }, 'query'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_TOKEN_NAMES_URL: 'names.example.org' }, 'not a URL'],
    [{ KOB_PAGES_INDEXER_URL: '   ' }, 'KOB_PAGES_INDEXER_URL is empty'],
    [{ KOB_PAGES_INDEXER_URL: 'http://idx.example.org' }, 'must use https'],
    [{ KOB_PAGES_INDEXER_URL: 'wss://idx.example.org' }, 'must use https'],
    [{ KOB_PAGES_INDEXER_URL: 'https://localhost:8090' }, 'not a public host'],
    [{ KOB_PAGES_INDEXER_URL: 'https://127.0.0.1:8090' }, 'not a public host'],
    [{ KOB_PAGES_INDEXER_URL: 'https://192.168.1.10' }, 'not a public host'],
    [{ KOB_PAGES_INDEXER_URL: 'https://[::1]:8090' }, 'not a public host'],
    [{ KOB_PAGES_INDEXER_URL: 'https://idx.example.org/?apikey=1' }, 'query'],
    [{ KOB_PAGES_INDEXER_URL: 'https://u:p@idx.example.org' }, 'password'],
    [{ KOB_PAGES_INDEXER_URL: 'idx.example.org' }, 'not a URL'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_NODE_URL: 'ws://node.example.org:17210' }, 'must use wss'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_NODE_URL: 'https://node.example.org' }, 'must use wss'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_EXTRA_INDEXER_URLS: 'https://IDX.example.org:443/' }, 'already listed'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_EXTRA_INDEXER_URLS: 'https://a.example,https://b.example,https://c.example,https://d.example,https://e.example' }, 'at most 4'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_EXPLORER_URL: 'http://explorer.example.org' }, 'must use https'],
    [{ KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_NETWORK: 'testnet-11' }, 'testnet-10 or mainnet'],
  ])('refuses %j', (env, message) => {
    const { config, errors } = pagesConfig(env);
    expect(config).toBeNull();
    expect(errors.join('\n')).toContain(message);
  });

  it('testnet-10 ships the supported programs with no token entries; the app accepts the file for testnet-10', () => {
    const shipped = JSON.parse(readFileSync(DEFAULT_REGISTRY_PATH, 'utf8'));
    const text = programRegistry(JSON.stringify(shipped), 'testnet-10');
    const doc = JSON.parse(text);
    expect(doc.network).toBe('testnet-10');
    expect(doc.tokens).toEqual([]);
    expect(doc.$schema).toBeUndefined();
    expect(doc.templates).toEqual(shipped.templates);
    const reg = parseRegistry(text, { kob: loadKobNode(), network: 'testnet-10' });
    expect(reg.templateChecks.length).toBe(shipped.templates.length);
    expect(() => parseRegistry(text, { kob: loadKobNode(), network: 'mainnet' })).toThrow();
  });

  it('a single indexer is allowed on both networks (the start price of a market order rests on the node check; a second indexer is an optional extra)', () => {
    expect(pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_INDEXER_URL: IDX }).errors).toEqual([]);
    expect(pagesConfig({ KOB_PAGES_NETWORK: 'testnet-10', KOB_PAGES_INDEXER_URL: IDX }).errors).toEqual([]);
    // the primary listed again is still no second indexer
    expect(pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_INDEXER_URL: IDX, KOB_PAGES_EXTRA_INDEXER_URLS: IDX }).config).toBeNull();
  });

  it('writes config.json, and the program list only on testnet-10 (mainnet keeps the pinned registry of build:release)', () => {
    const dir = mkdtempSync(join(tmpdir(), 'kob-pages-'));
    try {
      const tn = writeDeployment(dir, { network: 'testnet-10', indexerUrl: IDX, extraIndexerUrls: [], nodeUrl: '' });
      expect(tn.map((f) => f.path)).toEqual(['config.json', 'registry/tokens.json']);
      expect(JSON.parse(readFileSync(join(dir, 'config.json'), 'utf8')).indexerUrl).toBe(IDX);
      writeFileSync(join(dir, 'registry', 'tokens.json'), 'pinned');
      const main = writeDeployment(dir, { network: 'mainnet', indexerUrl: IDX, extraIndexerUrls: [], nodeUrl: '' });
      expect(main.map((f) => f.path)).toEqual(['config.json']);
      expect(readFileSync(join(dir, 'registry', 'tokens.json'), 'utf8')).toBe('pinned');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

// ------------------------------------------------------------------------------------------------ the post-deploy check

type Rule = { pattern: string; set: [string, string][]; detach: string[] };

/** public/_headers as rules (Cloudflare Pages format: a path line, then indented `Name: value` or `! Name` lines). */
function parseHeaders(text: string): Rule[] {
  const rules: Rule[] = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim() || line.trim().startsWith('#')) continue;
    if (!/^\s/.test(line)) rules.push({ pattern: line.trim(), set: [], detach: [] });
    else if (line.trim().startsWith('!')) rules.at(-1)!.detach.push(line.trim().slice(1).trim().toLowerCase());
    else {
      const [name, ...rest] = line.trim().split(':');
      rules.at(-1)!.set.push([name!.trim().toLowerCase(), rest.join(':').trim()]);
    }
  }
  return rules;
}

const matches = (pattern: string, path: string) => (pattern.endsWith('*') ? path.startsWith(pattern.slice(0, -1)) : path === pattern);

/** A static host like Cloudflare Pages: pretty URLs (`/index.html` -> `/`, `/signer.html` -> `/signer`), `_headers` applied (`detach: false`: a host that ignores `! Name`). */
function pagesHost(dist: string, headers: string, o: { detach?: boolean; tamper?: string } = {}): Server {
  const rules = parseHeaders(headers);
  return createServer((req, res) => {
    const path = new URL(req.url ?? '/', 'http://x').pathname;
    if (path === '/index.html') return void res.writeHead(308, { location: '/' }).end();
    if (path === '/signer.html') return void res.writeHead(308, { location: '/signer' }).end();
    const file = path === '/' ? 'index.html' : path === '/signer' ? 'signer.html' : path.slice(1);
    let body: Buffer;
    try {
      body = readFileSync(join(dist, file));
    } catch {
      return void res.writeHead(404).end();
    }
    if (o.tamper === file) body = Buffer.concat([body, Buffer.from('<script src="https://analytics.example/beacon.js"></script>')]);
    const h = new Map<string, string>();
    for (const r of rules.filter((r) => matches(r.pattern, path))) {
      if (o.detach !== false) for (const d of r.detach) h.delete(d);
      for (const [k, v] of r.set) h.set(k, h.has(k) ? `${h.get(k)}, ${v}` : v);
    }
    res.writeHead(200, Object.fromEntries(h)).end(body);
  });
}

describe('check-pages: the deployed headers and files', () => {
  const HEADERS = readFileSync(fileURLToPath(new URL('../public/_headers', import.meta.url)), 'utf8');
  let dist: string;
  const servers: Server[] = [];
  const serve = async (s: Server): Promise<string> => {
    servers.push(s);
    await new Promise<void>((r) => s.listen(0, '127.0.0.1', () => r()));
    const a = s.address();
    return `http://127.0.0.1:${typeof a === 'object' && a ? a.port : 0}/`;
  };

  beforeAll(() => {
    dist = mkdtempSync(join(tmpdir(), 'kob-pages-dist-'));
    mkdirSync(join(dist, 'assets'));
    writeFileSync(join(dist, 'index.html'), '<!doctype html><script type="module" src="./assets/main-1.js"></script>');
    writeFileSync(join(dist, 'signer.html'), '<!doctype html><title>signer</title>');
    writeFileSync(join(dist, 'assets', 'main-1.js'), 'console.log(1)');
    writeFileSync(join(dist, 'config.json'), '{}\n');
    writeFileSync(join(dist, '_headers'), HEADERS);
  });
  afterAll(async () => {
    await Promise.all(servers.map((s) => new Promise((r) => s.close(r))));
    rmSync(dist, { recursive: true, force: true });
  });

  it('passes on a host that applies public/_headers and serves the build unchanged', async () => {
    const base = await serve(pagesHost(dist, HEADERS));
    const { ok, lines } = await checkPages(base, { dist, attempts: 1 });
    expect(lines.filter((l) => l.startsWith('FAIL'))).toEqual([]);
    expect(ok).toBe(true);
    expect(lines.join('\n')).toContain('all 4 built files are served byte for byte');
  });

  it('fails when the signer page keeps the app policy (a host that ignores `! Header`)', async () => {
    const base = await serve(pagesHost(dist, HEADERS, { detach: false }));
    const { ok, lines } = await checkPages(base, { dist, attempts: 1 });
    expect(ok).toBe(false);
    expect(lines.join('\n')).toContain("/signer.html carries the app's frame-ancestors 'none'");
  });

  it("fails without frame-ancestors 'none' / X-Frame-Options DENY on the app", async () => {
    const weak = HEADERS.replace("; frame-ancestors 'none'", '').replace('  X-Frame-Options: DENY\n', '');
    const base = await serve(pagesHost(dist, weak));
    const { ok, lines } = await checkPages(base, { attempts: 1 });
    expect(ok).toBe(false);
    expect(lines.filter((l) => l.startsWith('FAIL')).map((l) => l.split(':')[0])).toEqual(['FAIL  / content-security-policy', 'FAIL  / x-frame-options']);
  });

  it('fails when a served file differs from the build (an injected script)', async () => {
    const base = await serve(pagesHost(dist, HEADERS, { tamper: 'index.html' }));
    const { ok, lines } = await checkPages(base, { dist, attempts: 1 });
    expect(ok).toBe(false);
    expect(lines.join('\n')).toContain('/index.html served bytes differ from the build');
  });

  it('fails when the host is unreachable', async () => {
    const { ok, lines } = await checkPages('http://127.0.0.1:9/', { attempts: 1 });
    expect(ok).toBe(false);
    expect(lines[0]).toContain('unreachable');
  });
});

describe('the deploy workflow gate (the signer page it ships takes private keys)', () => {
  const wf = readFileSync(fileURLToPath(new URL('../../.github/workflows/deploy-pages.yml', import.meta.url)), 'utf8');
  const job = (name: string): string => {
    const start = wf.indexOf(`\n  ${name}:\n`);
    expect(start, `job ${name}`).toBeGreaterThan(0);
    const next = wf.slice(start + 1).search(/\n  [a-z][a-z-]*:\n/);
    return next < 0 ? wf.slice(start) : wf.slice(start, start + 1 + next);
  };

  it('deploys only from master: a run of another ref fails first, and the deploy job checks the ref too', () => {
    expect(job('config')).toMatch(/- name: Production deploys only from master\n\s+if: github\.ref != 'refs\/heads\/master'/);
    expect(job('deploy')).toContain("github.ref == 'refs/heads/master'");
  });

  it('the deploy job runs in the environment production (a required reviewer approves there); the secret and variable names are unchanged', () => {
    expect(job('deploy')).toMatch(/\n    environment: production\n/);
    expect(job('config')).not.toContain('environment:');
    for (const name of ['CLOUDFLARE_API_TOKEN', 'CLOUDFLARE_ACCOUNT_ID']) expect(wf).toContain(`secrets.${name}`);
    for (const name of ['KOB_PAGES_PROJECT', 'KOB_PAGES_INDEXER_URL', 'KOB_PAGES_BRANCH', 'KOB_PAGES_NETWORK', 'KOB_PAGES_EXTRA_INDEXER_URLS', 'KOB_PAGES_NODE_URL', 'KOB_PAGES_EXPLORER_URL', 'KOB_PAGES_TOKEN_NAMES_URL']) expect(wf).toContain(`vars.${name}`);
  });
});
