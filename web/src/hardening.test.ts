// Hardening regressions that can be shown statically: CSP, SDK pin, node txid check.
import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { CSP_HEADER, CSP_META, META_DIRECTIVES } from '../csp.mjs';

const web = (rel: string) => fileURLToPath(new URL(`../${rel}`, import.meta.url));

describe('strict Content-Security-Policy and framing protection', () => {
  it('the policy denies by default and allows only the bundle, wasm compilation and the connections the app needs', () => {
    expect(META_DIRECTIVES['default-src']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['script-src']).toEqual(["'self'", "'wasm-unsafe-eval'"]); // no 'unsafe-inline', no 'unsafe-eval', no remote scripts
    expect(META_DIRECTIVES['object-src']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['base-uri']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['frame-src']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['form-action']).toEqual(["'none'"]);
    expect(CSP_META).not.toMatch(/frame-ancestors/); // not expressible in a <meta>
    expect(CSP_HEADER).toBe(`${CSP_META}; frame-ancestors 'none'`);
  });

  it('the build embeds the policy into index.html (vite plugin), and index.html has no inline script', () => {
    const cfg = readFileSync(web('vite.config.ts'), 'utf8');
    expect(cfg).toContain("'http-equiv': 'Content-Security-Policy'");
    expect(cfg).toContain('CSP_META');
    const html = readFileSync(web('index.html'), 'utf8');
    for (const m of html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)) expect(m[1]).toMatch(/\bsrc=/); // only external scripts
    expect(/<script\b(?![^>]*\bsrc=)[^>]*>\s*\S/i.test(html)).toBe(false);
  });

  it('public/_headers sends the same policy as a real header (with frame-ancestors) and the usual hardening headers', () => {
    expect(existsSync(web('public/_headers'))).toBe(true);
    const h = readFileSync(web('public/_headers'), 'utf8');
    expect(h).toContain(`Content-Security-Policy: ${CSP_HEADER}`);
    expect(h).toMatch(/X-Content-Type-Options: nosniff/);
    expect(h).toMatch(/X-Frame-Options: DENY/);
    expect(h).toMatch(/Referrer-Policy: no-referrer/);
  });

  it('the README documents the deployment headers and the connect-src pin', () => {
    const readme = readFileSync(web('README.md'), 'utf8');
    expect(readme).toMatch(/Content-Security-Policy/);
    expect(readme).toMatch(/frame-ancestors/);
    expect(readme).toMatch(/connect-src/);
  });
});

describe('the SDK download is pinned to a known hash', () => {
  it('fetch-sdk.mjs compares the zip hash with a pinned constant and refuses to unpack on a mismatch', () => {
    const src = readFileSync(web('scripts/fetch-sdk.mjs'), 'utf8');
    expect(src).toMatch(/EXPECTED_SHA256 = '[0-9a-f]{64}'/);
    expect(src).toMatch(/if \(sha !== EXPECTED_SHA256\) throw/);
    // the check comes before anything is unpacked or written
    expect(src.indexOf('sha !== EXPECTED_SHA256')).toBeLessThan(src.indexOf('unzipSync('));
    expect(src.indexOf('sha !== EXPECTED_SHA256')).toBeLessThan(src.indexOf("writeFileSync(join(VENDOR, 'SDK_SHA256.txt')"));
  });
});

describe('the node-reported txid is compared with the signed transaction id', () => {
  it('signAndSubmit refuses a node answer that is not signed.tx.id (behaviour tested in wallet/sign.test.ts)', () => {
    const src = readFileSync(web('src/wallet/sign.ts'), 'utf8');
    expect(src).toMatch(/txid !== signed\.tx\.id/);
  });
});
