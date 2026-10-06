// Unit tests of the e2e infrastructure helpers that do not need a browser: the static fixture server, the real-wallet helper library
// (extension unpacking, .env handling, popup routing, throw-away key derivation) and the UI test-id contract.
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { zipSync, strToU8 } from 'fflate';
import { afterAll, describe, expect, it } from 'vitest';
import { DEFAULT_MOUNTS, resolveStatic, startStaticServer } from '../e2e/serve-static.mjs';
import * as testids from '../e2e/testids';
import * as real from '../e2e-real/common.mjs';

// captured BEFORE any helper runs: importing the library must not have created its working directories
const dirsAtImport = [real.OUT_DIR, real.PROFILE_ROOT, real.EXT_ROOT].map((d) => existsSync(d));

const tmp = mkdtempSync(join(tmpdir(), 'kob-e2e-'));
afterAll(() => rmSync(tmp, { recursive: true, force: true }));

describe('static fixture server', () => {
  it('serves the placeholder page, the SDK and the kob-wasm web bindings with the right types, and nothing outside its roots', async () => {
    const s = await startStaticServer({ port: 0 });
    try {
      const html = await fetch(`${s.url}/`);
      expect(html.status).toBe(200);
      expect(html.headers.get('content-type')).toMatch(/text\/html/);
      expect(await html.text()).toContain('wallet-connect-kasware');
      expect((await fetch(`${s.url}/sdk/kaspa_bg.wasm`)).headers.get('content-type')).toBe('application/wasm');
      expect((await fetch(`${s.url}/kob/kob_wasm.js`)).headers.get('content-type')).toMatch(/javascript/);
      expect((await fetch(`${s.url}/nope.js`)).status).toBe(404);
      expect((await fetch(`${s.url}/`, { method: 'POST' })).status).toBe(404);
      for (const evil of ['/sdk/../../package.json', '/%2e%2e/%2e%2e/package.json', '/sdk/..%2f..%2fpackage.json', '/..%5c..%5cpackage.json']) {
        const r = await fetch(`${s.url}${evil}`);
        expect(r.status, evil).toBe(404);
      }
    } finally {
      await s.close();
    }
  });

  it('resolves against the mounts in order and refuses traversal and NUL bytes', () => {
    expect(resolveStatic('/', DEFAULT_MOUNTS)).toMatch(/fixtures-site[\\/]index\.html$/);
    expect(resolveStatic('/app.js', DEFAULT_MOUNTS)).toMatch(/fixtures-site[\\/]app\.js$/);
    expect(resolveStatic('/sdk/kaspa.js', DEFAULT_MOUNTS)).toMatch(/kaspa-web[\\/]kaspa\.js$/);
    expect(resolveStatic('/sdk/../../package.json', DEFAULT_MOUNTS)).toBeNull();
    expect(resolveStatic('/x\0y', DEFAULT_MOUNTS)).toBeNull();
    expect(resolveStatic('/%E0%A4%A', DEFAULT_MOUNTS)).toBeNull();
  });
});

describe('UI test-id contract', () => {
  it('lists every id of the brief exactly once, kebab-case, without duplicates', () => {
    const ids = Object.values(testids.TESTID);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.every((i) => /^[a-z]+(-[a-z]+)*$/.test(i))).toBe(true);
    const brief =
      'nav-market nav-orders nav-issue nav-settings wallet-connect-kasware wallet-connect-kaspire wallet-connect-kastle wallet-address wallet-network market-base-select book-asks book-bids trades-list order-type order-side-buy order-side-sell order-amount order-price order-tip order-review order-issues confirm-screen confirm-summary confirm-blocking confirm-sign confirm-cancel tx-status tx-id orders-list orders-cancel-all orders-export orders-import balances-panel issue-name issue-ticker issue-decimals issue-supply issue-review'.split(' ');
    for (const id of brief) expect(ids, id).toContain(id);
    expect(testids.orderRow('ab')).toBe('order-row-ab');
    expect(testids.orderCancel('ab')).toBe('order-cancel-ab');
    expect(testids.orderReplace('ab')).toBe('order-replace-ab');
    expect(testids.field('minFill')).toBe('field-minFill');
  });
});

describe('real-wallet helpers (e2e-real/common.mjs)', () => {
  it('has no side effects on import: no directories are created', () => {
    // the strong form: importing creates nothing, so a dir that did not exist before the import still does not exist after it
    dirsAtImport.forEach((existed, i) => {
      const dir = [real.OUT_DIR, real.PROFILE_ROOT, real.EXT_ROOT][i];
      if (!existed) expect(existsSync(dir), dir).toBe(false);
    });
  });

  it('unpacks a zip and a CRX3 / CRX2 wrapper of it, and refuses entries that escape the target', () => {
    const zip = zipSync({ 'manifest.json': strToU8('{"version":"1.2.3"}'), 'sub/a.js': strToU8('x') });
    const dir = join(tmp, 'zip');
    expect(real.unpackZip(zip, dir).sort()).toEqual(['manifest.json', 'sub/a.js']);
    expect(real.extensionVersion(dir)).toBe('1.2.3');
    const crx3 = Buffer.concat([Buffer.from('Cr24'), Buffer.from([3, 0, 0, 0]), Buffer.from([5, 0, 0, 0]), Buffer.from('HEADR'), Buffer.from(zip)]);
    const d3 = join(tmp, 'crx3');
    real.unpackCrx(crx3, d3);
    expect(real.extensionVersion(d3)).toBe('1.2.3');
    const crx2 = Buffer.concat([Buffer.from('Cr24'), Buffer.from([2, 0, 0, 0]), Buffer.from([2, 0, 0, 0]), Buffer.from([3, 0, 0, 0]), Buffer.from('PK'), Buffer.from('SIG'), Buffer.from(zip)]);
    const d2 = join(tmp, 'crx2');
    real.unpackCrx(crx2, d2);
    expect(readFileSync(join(d2, 'sub', 'a.js'), 'utf8')).toBe('x');
    expect(() => real.crxToZip(Buffer.from('NOPE0000000000'))).toThrow(/not a CRX/);
    expect(() => real.crxToZip(Buffer.concat([Buffer.from('Cr24'), Buffer.from([9, 0, 0, 0]), Buffer.alloc(8)]))).toThrow(/unsupported CRX version/);
    const evil = zipSync({ '../evil.txt': strToU8('x') });
    expect(() => real.unpackZip(evil, join(tmp, 'evil'))).toThrow(/escapes/);
    expect(existsSync(join(tmp, 'evil.txt'))).toBe(false);
  });

  it('reads and upserts the gitignored .env without leaking other keys', () => {
    const f = join(tmp, '.env');
    writeFileSync(f, '# comment\nA=1\nB = "two"\n');
    expect(real.loadEnv(f)).toEqual({ A: '1', B: 'two' });
    real.upsertEnv({ B: '3', C: 'x y' }, f);
    expect(real.loadEnv(f)).toEqual({ A: '1', B: '3', C: 'x y' });
    expect(readFileSync(f, 'utf8')).toContain('# comment');
    expect(real.loadEnv(join(tmp, 'missing'))).toEqual({});
    const phrase = real.ensureMnemonic('WALLET_MNEMONIC_TEST', f);
    expect(phrase.split(' ')).toHaveLength(12);
    expect(real.ensureMnemonic('WALLET_MNEMONIC_TEST', f)).toBe(phrase); // stable across runs
  });

  it("derives KasWare's address from a throw-away mnemonic (Schnorr P2PK, testnet + mainnet)", () => {
    const phrase = real.randomMnemonic();
    expect(phrase.split(' ')).toHaveLength(12);
    expect(real.randomMnemonic()).not.toBe(phrase);
    expect(real.kaswareAddress(phrase)).toMatch(/^kaspatest:q[a-z0-9]{60,}$/);
    expect(real.kaswareAddress(phrase, 'mainnet')).toMatch(/^kaspa:q/);
    expect(real.kaswareAddress(phrase)).toBe(real.kaswareAddress(phrase));
  });

  it("recognises each wallet's approval window by URL", () => {
    const page = (url: string, closed = false) => ({ url: () => url, isClosed: () => closed });
    expect(real.isWalletPopup('kasware', page('chrome-extension://abc/notification.html#/sign'))).toBe(true);
    expect(real.isWalletPopup('kasware', page('chrome-extension://abc/index.html'))).toBe(false);
    expect(real.isWalletPopup('kaspire', page('chrome-extension://abc/approval.html?id=1'))).toBe(true);
    expect(real.isWalletPopup('kastle', page('chrome-extension://abc/popup.html?requestId=7#/sign'))).toBe(true);
    expect(real.isWalletPopup('kastle', page('chrome-extension://abc/popup.html'))).toBe(false);
    expect(real.isWalletPopup('kaspire', page('chrome-extension://abc/approval.html', true))).toBe(false);
  });

  it('approvePopups hands every new popup to the handler exactly once and reports errors', async () => {
    const mk = (url: string) => ({ url: () => url, isClosed: () => false });
    const pages = [mk('chrome-extension://a/notification.html#1')];
    const ctx = { pages: () => pages };
    const calls: string[] = [];
    const watch = real.approvePopups(
      ctx,
      async (p: { url(): string }, info: { index: number }) => {
        calls.push(`${info.index}:${p.url()}`);
        if (info.index === 1) throw new Error('boom');
        return 'clicked';
      },
      { wallet: 'kasware', pollMs: 10 },
    );
    await new Promise((r) => setTimeout(r, 60));
    pages.push(mk('chrome-extension://a/notification.html#2'), mk('chrome-extension://a/index.html'));
    await new Promise((r) => setTimeout(r, 60));
    const seen = await watch.stop();
    expect(calls).toEqual(['0:chrome-extension://a/notification.html#1', '1:chrome-extension://a/notification.html#2']);
    expect(seen[0]).toMatchObject({ index: 0, result: 'clicked' });
    expect(seen[1]).toMatchObject({ index: 1, error: 'boom' });
    expect(() => real.approvePopups(ctx, () => {}, { wallet: 'metamask' as never })).toThrow(/unknown wallet/);
  });

  it('withPopupApproval returns the start result together with the popups seen', async () => {
    const mk = (url: string) => ({ url: () => url, isClosed: () => false });
    const pages = [mk('chrome-extension://a/approval.html')];
    const out = await real.withPopupApproval(
      { pages: () => pages },
      'kaspire',
      async () => {
        await new Promise((r) => setTimeout(r, 80));
        return 42;
      },
      () => 'approved',
      { pollMs: 10 },
    );
    expect(out.result).toBe(42);
    expect(out.popups).toHaveLength(1);
    expect(out.popups[0].result).toBe('approved');
  });

  it('ensureExtension reuses the unpacked cache, copies the wallet-gate cache, and only downloads when allowed', async () => {
    const vendor = join(tmp, 'vendor');
    const extRoot = join(tmp, 'ext-root');
    real.unpackZip(zipSync({ 'manifest.json': strToU8('{"version":"0.10.0"}'), 'bg.js': strToU8('1') }), join(vendor, 'ext-kasware', 'unpacked'));
    const first = await real.ensureExtension('kasware', { extRoot, vendors: [join(tmp, 'nowhere'), vendor], download: false });
    expect(first).toMatchObject({ source: 'vendor', version: '0.10.0', dir: join(extRoot, 'kasware') });
    expect(readFileSync(join(first.dir, 'bg.js'), 'utf8')).toBe('1');
    // the second call reuses the copy even without any vendor cache
    expect(await real.ensureExtension('kasware', { extRoot, vendors: [], download: false })).toMatchObject({ source: 'cache', version: '0.10.0' });
    await expect(real.ensureExtension('kaspire', { extRoot, vendors: [vendor], download: false })).rejects.toThrow(/not found/);
    await expect(real.ensureExtension('metamask' as never, { extRoot })).rejects.toThrow(/unknown wallet/);
  });

  it('lists all three wallets with a cache dir under the wallet-gate vendor layout', () => {
    expect(Object.keys(real.EXTENSIONS)).toEqual(['kasware', 'kaspire', 'kastle']);
    expect(real.EXTENSIONS.kasware.vendorDir).toBe('ext-kasware/unpacked');
    expect(real.EXTENSIONS.kaspire.zipSha256).toMatch(/^[0-9a-f]{64}$/);
    expect(real.EXT_ROOT.replace(/\\/g, '/')).toMatch(/web\/e2e-real\/\.ext$/);
  });
});
