// The public deployment's helpers (.github/workflows/deploy-pages.yml): config.json from the KOB_PAGES_* variables (scripts/pages-config.mjs)
// and the post-deploy check (scripts/check-pages.mjs), run against a local host that applies public/_headers the way Cloudflare Pages does.
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { createServer, type Server } from 'node:http';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { noindexDist, pagesConfig, programRegistry, writeDeployment } from '../scripts/pages-config.mjs';
import { checkPages, checkProtected } from '../scripts/check-pages.mjs';
import { ensureDomain, hostOf, pagesDevHost, zoneCandidates } from '../scripts/pages-domain.mjs';
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

  it('landing-only (the mainnet front page): no indexer is required, the other variables are ignored with a note, and no config.json is written', () => {
    const r = pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_LANDING_ONLY: '1' });
    expect(r.errors).toEqual([]);
    expect(r.config).toEqual({ network: 'mainnet', landingOnly: true });
    for (const v of ['1', 'true', ' TRUE ']) expect(pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_LANDING_ONLY: v }).config).toEqual({ network: 'mainnet', landingOnly: true });
    // a stale indexer variable is not validated, it is ignored (the page connects to nothing)
    const stale = pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_LANDING_ONLY: '1', KOB_PAGES_INDEXER_URL: 'http://localhost:1', KOB_PAGES_NODE_URL: 'nonsense' });
    expect(stale.errors).toEqual([]);
    expect(stale.notes.join(' ')).toContain('KOB_PAGES_INDEXER_URL, KOB_PAGES_NODE_URL are ignored');
    // off (empty, 0, false): the app, which needs its indexer as before
    for (const v of ['', '0', 'false']) {
      const app = pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_LANDING_ONLY: v });
      expect(app.config).toBeNull();
      expect(app.errors.join(' ')).toContain('KOB_PAGES_INDEXER_URL is empty');
    }
    // a typo is an error, and so is a bad network
    expect(pagesConfig({ KOB_PAGES_NETWORK: 'mainnet', KOB_PAGES_LANDING_ONLY: 'yes' }).errors.join(' ')).toContain('KOB_PAGES_LANDING_ONLY');
    expect(pagesConfig({ KOB_PAGES_NETWORK: 'nope', KOB_PAGES_LANDING_ONLY: '1' }).config).toBeNull();
    const dir = mkdtempSync(join(tmpdir(), 'kob-pages-'));
    try {
      expect(writeDeployment(dir, r.config!)).toEqual([]);
      expect(existsSync(join(dir, 'config.json'))).toBe(false);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
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

describe('pages-domain: the custom domain of a Pages site', () => {
  type Route = { status?: number; body: unknown };
  /** A fake Cloudflare API: `routes` maps "METHOD path" to the answer; `calls` records what was asked. */
  const api = (routes: Record<string, Route>) => {
    const calls: string[] = [];
    const fetchFn = (async (url: string, init?: RequestInit) => {
      const key = `${init?.method ?? 'GET'} ${String(url).replace('https://api.cloudflare.com/client/v4', '')}`;
      calls.push(key);
      const r = routes[key] ?? { status: 403, body: { success: false, errors: [{ code: 10000, message: 'Authentication error' }] } };
      return { ok: (r.status ?? 200) < 400, status: r.status ?? 200, json: async () => r.body };
    }) as unknown as typeof fetch;
    return { fetchFn, calls };
  };
  const opts = { token: 't', account: 'acc', project: 'kob-web', host: 'kob-tn10.kaspa.ag' };
  const domains = '/accounts/acc/pages/projects/kob-web/domains';

  it('reads the host of the public URL (https only) and the candidate zones, longest first', () => {
    expect(hostOf('https://kob-tn10.kaspa.ag/')).toBe('kob-tn10.kaspa.ag');
    expect(hostOf('http://kob-tn10.kaspa.ag')).toBeNull();
    expect(hostOf('nonsense')).toBeNull();
    expect(zoneCandidates('a.b.kaspa.ag')).toEqual(['a.b.kaspa.ag', 'b.kaspa.ag', 'kaspa.ag']);
  });

  it('a domain that is attached with its CNAME needs nothing', async () => {
    const { fetchFn, calls } = api({
      [`GET ${domains}`]: { body: { success: true, result: [{ name: 'kob-tn10.kaspa.ag', status: 'active' }] } },
      'GET /zones?name=kob-tn10.kaspa.ag&account.id=acc': { body: { success: true, result: [] } },
      'GET /zones?name=kaspa.ag&account.id=acc': { body: { success: true, result: [{ id: 'z1', name: 'kaspa.ag' }] } },
      'GET /zones/z1/dns_records?name=kob-tn10.kaspa.ag': { body: { success: true, result: [{ type: 'CNAME', content: 'kob-web.pages.dev' }] } },
    });
    const r = await ensureDomain({ ...opts, fetchFn });
    expect(r.ok).toBe(true);
    expect(calls.some((c) => c.startsWith('POST'))).toBe(false);
  });

  it('attaches a missing domain and, with a token that may edit DNS, creates the proxied CNAME', async () => {
    const { fetchFn, calls } = api({
      [`GET ${domains}`]: { body: { success: true, result: [] } },
      [`POST ${domains}`]: { body: { success: true, result: { status: 'pending' } } },
      'GET /zones?name=kob-tn10.kaspa.ag&account.id=acc': { body: { success: true, result: [{ id: 'z1', name: 'kob-tn10.kaspa.ag' }] } },
      'GET /zones/z1/dns_records?name=kob-tn10.kaspa.ag': { body: { success: true, result: [] } },
      'POST /zones/z1/dns_records': { body: { success: true, result: {} } },
    });
    const r = await ensureDomain({ ...opts, fetchFn });
    expect(r.ok).toBe(true);
    expect(calls).toContain(`POST ${domains}`);
    expect(calls).toContain('POST /zones/z1/dns_records');
  });

  it('a token limited to Pages: the domain is attached and the step says which CNAME to add (still a success)', async () => {
    const { fetchFn } = api({
      [`GET ${domains}`]: { body: { success: true, result: [] } },
      [`POST ${domains}`]: { body: { success: true, result: { status: 'pending' } } },
    });
    const r = await ensureDomain({ ...opts, fetchFn });
    expect(r.ok).toBe(true);
    expect(r.lines.join('\n')).toContain('`CNAME kob-tn10.kaspa.ag -> kob-web.pages.dev`');
  });

  describe('a project whose plain <project>.pages.dev name is taken (its address carries a suffix)', () => {
    const project = '/accounts/acc/pages/projects/kob-web';
    const info = { [`GET ${project}`]: { body: { success: true, result: { name: 'kob-web', subdomain: 'kob-web-bs6.pages.dev' } } } };

    it('reads the address from the API: a suffixed subdomain, a bare label, or nothing usable', () => {
      expect(pagesDevHost('kob-web-bs6.pages.dev')).toBe('kob-web-bs6.pages.dev');
      expect(pagesDevHost(' KOB-Web-BS6.pages.dev. ')).toBe('kob-web-bs6.pages.dev');
      expect(pagesDevHost('kob-web-bs6')).toBe('kob-web-bs6.pages.dev');
      for (const bad of [undefined, null, '', 'evil.example.com', 'a b.pages.dev']) expect(pagesDevHost(bad), String(bad)).toBeNull();
    });

    it('creates the CNAME to the real address, never to the plain name (which another party holds: Cloudflare error 1014)', async () => {
      const { fetchFn, calls } = api({
        ...info,
        [`GET ${domains}`]: { body: { success: true, result: [] } },
        [`POST ${domains}`]: { body: { success: true, result: { status: 'pending' } } },
        'GET /zones?name=kob-tn10.kaspa.ag&account.id=acc': { body: { success: true, result: [{ id: 'z1', name: 'kob-tn10.kaspa.ag' }] } },
        'GET /zones/z1/dns_records?name=kob-tn10.kaspa.ag': { body: { success: true, result: [] } },
        'POST /zones/z1/dns_records': { body: { success: true, result: {} } },
      });
      const bodies: unknown[] = [];
      const wrapped = (async (url: string, init?: RequestInit) => {
        if (init?.method === 'POST' && String(url).endsWith('/dns_records')) bodies.push(JSON.parse(String(init.body)));
        return fetchFn(url, init);
      }) as unknown as typeof fetch;
      const r = await ensureDomain({ ...opts, fetchFn: wrapped });
      expect(r.ok).toBe(true);
      expect(calls).toContain(`GET ${project}`);
      expect(bodies).toEqual([{ type: 'CNAME', name: 'kob-tn10.kaspa.ag', content: 'kob-web-bs6.pages.dev', proxied: true, ttl: 1 }]);
      expect(r.lines.join('\n')).toContain('created `CNAME kob-tn10.kaspa.ag -> kob-web-bs6.pages.dev`');
      expect(r.lines.join('\n')).not.toContain('could not be read');
    });

    it('a token limited to Pages: the printed dashboard record names the real address', async () => {
      const { fetchFn } = api({ ...info, [`GET ${domains}`]: { body: { success: true, result: [] } }, [`POST ${domains}`]: { body: { success: true, result: { status: 'pending' } } } });
      const r = await ensureDomain({ ...opts, fetchFn });
      expect(r.ok).toBe(true);
      expect(r.lines.join('\n')).toContain('`CNAME kob-tn10.kaspa.ag -> kob-web-bs6.pages.dev`');
      expect(r.lines.join('\n')).not.toContain('-> kob-web.pages.dev');
    });

    it('an existing CNAME to the plain name is wrong (it fails the step), one to the real address is right', async () => {
      const routes = (content: string) => ({
        ...info,
        [`GET ${domains}`]: { body: { success: true, result: [{ name: 'kob-tn10.kaspa.ag', status: 'active' }] } },
        'GET /zones?name=kob-tn10.kaspa.ag&account.id=acc': { body: { success: true, result: [{ id: 'z1', name: 'kob-tn10.kaspa.ag' }] } },
        'GET /zones/z1/dns_records?name=kob-tn10.kaspa.ag': { body: { success: true, result: [{ type: 'CNAME', content }] } },
      });
      const wrong = await ensureDomain({ ...opts, fetchFn: api(routes('kob-web.pages.dev')).fetchFn });
      expect(wrong.ok).toBe(false);
      expect(wrong.lines.join('\n')).toContain('not kob-web-bs6.pages.dev');
      const right = await ensureDomain({ ...opts, fetchFn: api(routes('kob-web-bs6.pages.dev')).fetchFn });
      expect(right.ok).toBe(true);
      expect(right.lines.join('\n')).toContain('`CNAME kob-tn10.kaspa.ag -> kob-web-bs6.pages.dev` exists');
    });

    it('when the API cannot say, the plain name is assumed and the summary says to copy the real one', async () => {
      const { fetchFn } = api({ [`GET ${domains}`]: { body: { success: true, result: [] } }, [`POST ${domains}`]: { body: { success: true, result: { status: 'pending' } } } });
      const r = await ensureDomain({ ...opts, fetchFn });
      expect(r.lines.join('\n')).toContain('address could not be read from the API');
      expect(r.lines.join('\n')).toContain('copy the real one from the project page');
    });
  });

  it('a domain held by another project is not moved: the step fails and says to remove it there first', async () => {
    const { fetchFn, calls } = api({
      [`GET ${domains}`]: { body: { success: true, result: [] } },
      [`POST ${domains}`]: { status: 409, body: { success: false, errors: [{ code: 8000018, message: 'domain already in use' }] } },
    });
    const r = await ensureDomain({ ...opts, fetchFn });
    expect(r.ok).toBe(false);
    expect(r.lines.join('\n')).toContain('remove it there first');
    expect(calls.some((c) => c.startsWith('DELETE'))).toBe(false);
  });
});

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

  it('landing-only: a site without a signer page passes with the flag (the app policy on /, the files byte for byte), and fails without it', async () => {
    const landing = mkdtempSync(join(tmpdir(), 'kob-pages-landing-'));
    try {
      mkdirSync(join(landing, 'assets'));
      writeFileSync(join(landing, 'index.html'), '<!doctype html><script type="module" src="./assets/main-1.js"></script>');
      writeFileSync(join(landing, 'assets', 'main-1.js'), 'console.log(1)');
      writeFileSync(join(landing, '_headers'), HEADERS);
      const base = await serve(pagesHost(landing, HEADERS));
      const ok = await checkPages(base, { dist: landing, landingOnly: true, attempts: 1 });
      expect(ok.lines.filter((l) => l.startsWith('FAIL'))).toEqual([]);
      expect(ok.ok).toBe(true);
      expect(ok.lines.join('\n')).not.toContain('signer');
      expect(ok.lines.join('\n')).toContain('all 2 built files are served byte for byte');
      const app = await checkPages(base, { dist: landing, attempts: 1 });
      expect(app.ok).toBe(false);
      expect(app.lines.join('\n')).toContain('/signer.html answered HTTP 404');
    } finally {
      rmSync(landing, { recursive: true, force: true });
    }
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

  it('the mainnet site is a matrix entry only when its project variable is set (its indexer is optional, but not alone), and is always built with network mainnet', () => {
    expect(job('deploy')).toContain('matrix: ${{ fromJSON(needs.config.outputs.matrix) }}');
    expect(job('config')).toContain('if [ -z "$MAIN_PROJECT" ] && [ -n "$MAIN_INDEXER" ]; then');
    expect(job('config')).toContain('if [ -n "$MAIN_PROJECT" ]; then');
    expect(job('config')).not.toContain('must be set together');
    expect(job('config')).toContain('(if $mp != "" then [{site: "mainnet", project: $mp, network: "mainnet"');
    for (const name of ['KOB_PAGES_MAINNET_PROJECT', 'KOB_PAGES_MAINNET_INDEXER_URL', 'KOB_PAGES_MAINNET_PUBLIC_URL']) expect(wf).toContain(`vars.${name}`);
    // the wrangler commands keep their options apart (an earlier edit glued the project name to `--branch`)
    expect(wf).toMatch(/pages project create \$\{\{ matrix\.project \}\} --production-branch=/);
    expect(wf).toMatch(/--project-name=\$\{\{ matrix\.project \}\} --branch=/);
    // no deploy step reads a site's value from a variable directly: the matrix carries it
    expect(job('deploy')).not.toContain('vars.');
  });
});

describe('the mainnet site is the landing-only front page until mainnet launches (docs/ops/web-hosting.md, "The mainnet front page")', () => {
  const wf = readFileSync(fileURLToPath(new URL('../../.github/workflows/deploy-pages.yml', import.meta.url)), 'utf8');
  const ci = readFileSync(fileURLToPath(new URL('../../.github/workflows/ci.yml', import.meta.url)), 'utf8');

  it('the workflow file itself marks the mainnet entry landing-only (no variable can lift it) and the testnet entry is the app', () => {
    expect(wf).toContain('{site: "mainnet", project: $mp, network: "mainnet", landing: "1",');
    expect(wf).toContain('{site: "testnet", project: $p, network: $n, landing: "",');
    expect(wf).not.toMatch(/vars\.KOB_[A-Z_]*LANDING/);
    expect(wf.match(/{site: "[a-z]+", [^}]*landing: "/g)).toHaveLength(2);
  });

  it("the site's landing flag reaches the check of the variables, the release build, config.json, the Pages check and a landing e2e run", () => {
    expect(wf).toContain('KOB_PAGES_LANDING_ONLY=$(jq -r .landing <<< "$site")');
    expect(wf).toContain('KOB_LANDING_ONLY: ${{ matrix.landing }}');
    expect(wf).toContain('KOB_PAGES_LANDING_ONLY: ${{ matrix.landing }}');
    expect(wf).toMatch(/- if: matrix\.landing != ''\n\s+run: npm run e2e:landing/);
    expect(wf).toContain('if [ -n "$LANDING" ]; then flags+=(--landing-only); fi');
    expect(wf).toContain('node scripts/check-pages.mjs "$URL" --dist dist "${flags[@]}"');
    // the release bundle step is the one that gets the flag (the e2e build before it is the app's)
    const build = wf.slice(wf.indexOf('- run: npm run build:release'));
    expect(build.slice(0, build.indexOf('- name:'))).toContain('KOB_LANDING_ONLY');
    expect(wf.slice(0, wf.indexOf('- run: npm run build:release'))).not.toMatch(/KOB_LANDING_ONLY: /);
  });

  it('CI runs the landing-only e2e on every push, against its own build directory', () => {
    expect(ci).toContain('- run: npm run e2e:landing');
    const pkg = JSON.parse(readFileSync(fileURLToPath(new URL('../package.json', import.meta.url)), 'utf8')) as { scripts: Record<string, string> };
    expect(pkg.scripts['e2e:landing']).toBe('playwright test --config playwright.landing.config.ts');
    const cfg = readFileSync(fileURLToPath(new URL('../playwright.landing.config.ts', import.meta.url)), 'utf8');
    expect(cfg).toContain("env: { KOB_LANDING_ONLY: '1', KOB_NETWORK: 'mainnet' }");
    expect(cfg).toContain('--outDir dist-landing');
  });
});

describe('the internal mainnet app: a preview of the mainnet project behind Cloudflare Access (docs/ops/web-hosting.md, 4c)', () => {
  const wf = readFileSync(fileURLToPath(new URL('../../.github/workflows/deploy-pages.yml', import.meta.url)), 'utf8');
  const deploy = wf.slice(wf.indexOf('\n  deploy:\n'));

  it('is a third matrix entry only with KOB_PAGES_MAINNET_INTERNAL=1 and the mainnet project: the full mainnet app (not landing-only), on the branch `internal`, no public URL', () => {
    expect(wf).toContain('MAIN_INTERNAL: ${{ vars.KOB_PAGES_MAINNET_INTERNAL }}');
    expect(wf).toContain('(if $mp != "" and $mint == "1" then [{site: "mainnet-internal", project: $mp, network: "mainnet", landing: "", branch: "internal", internal: "1", indexer: $mi,');
    expect(wf).toContain('names: $mnm, public: ""}] else [] end))}');
    // the production entries upload to KOB_PAGES_BRANCH
    expect(wf.match(/branch: \$br, internal: ""/g)).toHaveLength(2);
    expect(deploy).toContain('--branch=${{ matrix.branch }}');
    expect(deploy).not.toContain('--branch=${{ needs.config.outputs.branch }}');
  });

  it('refuses a half-set configuration: not 1, no project or indexer, or a production branch that is `internal`', () => {
    expect(wf).toContain('if [ -n "$MAIN_INTERNAL" ] && [ "$MAIN_INTERNAL" != 1 ]; then');
    expect(wf).toContain('KOB_PAGES_MAINNET_INTERNAL=1 needs KOB_PAGES_MAINNET_PROJECT and KOB_PAGES_MAINNET_INDEXER_URL');
    expect(wf).toContain('if [ "$BRANCH" = internal ]; then');
  });

  it('the upload must come out as a preview (never the production deployment), and its check is the protection check, not the byte-for-byte one', () => {
    expect(deploy).toMatch(/if \[ -n "\$INTERNAL" \]; then[\s\S]*if \[ "\$PAGES_ENV" != preview \]; then/);
    expect(deploy).toContain('elif [ "$PAGES_ENV" != production ]; then');
    expect(deploy).toMatch(/Post-deploy check of the internal app[\s\S]*if: matrix\.internal != ''/);
    expect(deploy).toContain('node scripts/check-pages.mjs --protected "${urls[@]}"');
    expect(deploy).toContain('ALIAS: ${{ steps.deploy.outputs.pages-deployment-alias-url }}');
    // the ordinary check (headers, files) runs for the other sites only: an Access login page is not the app
    expect(deploy).toMatch(/- name: Post-deploy check \(headers of public\/_headers, every file byte for byte\)\n\s+if: matrix\.internal == ''/);
    // no custom domain for it (public is empty), and it is never given a public URL
    expect(deploy).toMatch(/Custom domain of the site[\s\S]*if: matrix\.public != ''/);
  });

  it('keeps the internal build out of search engines: robots.txt disallows everything, X-Robots-Tag noindex in its _headers; the source _headers is untouched', () => {
    expect(deploy).toContain('KOB_PAGES_INTERNAL: ${{ matrix.internal }}');
    const dir = mkdtempSync(join(tmpdir(), 'kob-internal-'));
    try {
      const headers = readFileSync(fileURLToPath(new URL('../public/_headers', import.meta.url)), 'utf8');
      writeFileSync(join(dir, '_headers'), headers);
      const files = noindexDist(dir);
      expect(files.map((f) => f.path)).toEqual(['_headers', 'robots.txt']);
      expect(readFileSync(join(dir, 'robots.txt'), 'utf8')).toBe('User-agent: *\nDisallow: /\n');
      const out = readFileSync(join(dir, '_headers'), 'utf8');
      expect(out.startsWith(headers.replace(/\s*$/, '\n'))).toBe(true); // the app's rules are all still there
      expect(out.endsWith('\n/*\n  X-Robots-Tag: noindex, nofollow\n')).toBe(true);
      expect(headers).not.toContain('X-Robots-Tag'); // the public build is not noindexed by this
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe('check-pages --protected: the internal app is never served openly', () => {
  const fake = (res: { status: number; location?: string } | Error) =>
    (async () => {
      if (res instanceof Error) throw res;
      return { status: res.status, headers: new Headers(res.location ? { location: res.location } : {}) };
    }) as unknown as typeof fetch;
  const URLS = ['https://abc123.kob-web-bs6.pages.dev', 'https://internal.kob-web-bs6.pages.dev'];

  it('passes when an unauthenticated visitor is sent to Cloudflare Access (any redirect status), or refused with 401 / 403', async () => {
    for (const status of [302, 307]) {
      const r = await checkProtected(URLS, { fetchFn: fake({ status, location: 'https://team.cloudflareaccess.com/cdn-cgi/access/login/abc?kid=1' }) });
      expect(r.ok, String(status)).toBe(true);
      expect(r.lines.every((l) => l.startsWith('ok'))).toBe(true);
    }
    expect((await checkProtected(URLS, { fetchFn: fake({ status: 302, location: '/cdn-cgi/access/login/x' }) })).ok).toBe(true);
    for (const status of [401, 403]) expect((await checkProtected(URLS, { fetchFn: fake({ status }) })).ok, String(status)).toBe(true);
  });

  it('fails when the app answers (200), when a redirect leads anywhere but Access, or on any other status', async () => {
    const open = await checkProtected(URLS, { fetchFn: fake({ status: 200 }) });
    expect(open.ok).toBe(false);
    expect(open.lines).toHaveLength(2);
    expect(open.lines[0]).toContain('the internal app is NOT protected');
    expect((await checkProtected(URLS, { fetchFn: fake({ status: 302, location: 'https://example.org/login' }) })).ok).toBe(false);
    expect((await checkProtected(URLS, { fetchFn: fake({ status: 302 }) })).ok).toBe(false);
    for (const status of [404, 500, 204]) expect((await checkProtected(URLS, { fetchFn: fake({ status }) })).ok, String(status)).toBe(false);
  });

  it('one open URL fails the check even when the other is protected (the branch alias and the deployment URL are separate hosts)', async () => {
    const mixed = (async (url: string) => (String(url).startsWith('https://internal.') ? { status: 200, headers: new Headers() } : { status: 403, headers: new Headers() })) as unknown as typeof fetch;
    const r = await checkProtected(URLS, { fetchFn: mixed });
    expect(r.ok).toBe(false);
    expect(r.lines.map((l) => l.split(' ')[0])).toEqual(['ok', 'FAIL']);
  });

  it('an unreachable URL is a failure (nothing is proven), after its retries', async () => {
    const r = await checkProtected([URLS[0]!], { fetchFn: fake(new Error('getaddrinfo ENOTFOUND')), attempts: 2, delayMs: 1 });
    expect(r.ok).toBe(false);
    expect(r.lines[0]).toContain('the protection cannot be confirmed');
  });
});
