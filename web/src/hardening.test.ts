// Hardening regressions that can be shown statically: CSP, SDK pin, node txid check.
import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { CSP_HEADER, CSP_META, META_DIRECTIVES, SIGNER_CSP_HEADER, SIGNER_CSP_META, SIGNER_META_DIRECTIVES } from '../csp.mjs';

const web = (rel: string) => fileURLToPath(new URL(`../${rel}`, import.meta.url));

describe('strict Content-Security-Policy and framing protection', () => {
  it('the policy denies by default and allows only the bundle, wasm compilation and the connections the app needs', () => {
    expect(META_DIRECTIVES['default-src']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['script-src']).toEqual(["'self'", "'wasm-unsafe-eval'"]); // no 'unsafe-inline', no 'unsafe-eval', no remote scripts
    expect(META_DIRECTIVES['object-src']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['base-uri']).toEqual(["'none'"]);
    expect(META_DIRECTIVES['frame-src']).toEqual(["'self'"]); // the app's own private-key signer frame only
    expect(META_DIRECTIVES['require-trusted-types-for']).toEqual(["'script'"]);
    expect(META_DIRECTIVES['trusted-types']).toEqual(['default']); // one policy, installed at boot (src/app/trusted-types.ts)
    expect(META_DIRECTIVES['form-action']).toEqual(["'none'"]);
    // token icons: the app fetches the bytes (connect-src), checks their hash and shows them from a blob: URL it made (data/kascov.ts); an <img> may NOT load a
    // third-party URL, so img-src never names a host or https:
    expect(META_DIRECTIVES['img-src']).toEqual(["'self'", 'data:', 'blob:']);
    expect(CSP_META).not.toMatch(/img-src[^;]*(https:|http:|\*)/);
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

describe('the private-key signer page has its own strict policy and is framed sandboxed', () => {
  it('no network beyond its own bundle, no inline styles, Trusted Types, framed only by the app (header)', () => {
    expect(SIGNER_META_DIRECTIVES['default-src']).toEqual(["'none'"]);
    expect(SIGNER_META_DIRECTIVES['script-src']).toEqual(["'self'", "'wasm-unsafe-eval'"]);
    expect(SIGNER_META_DIRECTIVES['connect-src']).toEqual(["'self'"]);
    expect(SIGNER_META_DIRECTIVES['style-src']).toEqual(["'self'"]);
    expect(SIGNER_META_DIRECTIVES['frame-src']).toEqual(["'none'"]);
    expect(SIGNER_META_DIRECTIVES['trusted-types']).toEqual(["'none'"]);
    expect(SIGNER_CSP_META).not.toMatch(/frame-ancestors/);
    expect(SIGNER_CSP_HEADER).toBe(`${SIGNER_CSP_META}; sandbox allow-scripts; frame-ancestors 'self'`);
    const h = readFileSync(web('public/_headers'), 'utf8');
    expect(h).toContain(`/signer.html\n  ! Content-Security-Policy\n  ! X-Frame-Options\n  Content-Security-Policy: ${SIGNER_CSP_HEADER}\n  X-Frame-Options: SAMEORIGIN`);
    expect(h).toMatch(/\/assets\/\*\n  Access-Control-Allow-Origin: \*/); // the sandboxed (opaque-origin) signer loads its modules and wasm with CORS
  });

  it('the build has the signer page as a second entry with its own policy; the frame is sandboxed WITHOUT allow-same-origin', () => {
    const cfg = readFileSync(web('vite.config.ts'), 'utf8');
    expect(cfg).toMatch(/signer: fileURLToPath\(new URL\('\.\/signer\.html'/);
    expect(cfg).toContain('SIGNER_CSP_META');
    const html = readFileSync(web('signer.html'), 'utf8');
    expect(/<script\b(?![^>]*\bsrc=)[^>]*>\s*\S/i.test(html)).toBe(false);
    const frame = readFileSync(web('src/ui/shell/SignerFrame.tsx'), 'utf8');
    // the JSX attribute (own line in SignerFrame.tsx): scripts only, never allow-same-origin
    expect([...frame.matchAll(/^\s+sandbox="([^"]*)"$/gm)].map((m) => m[1])).toEqual(['allow-scripts']);
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
