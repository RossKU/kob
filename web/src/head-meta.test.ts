// The link-preview head of index.html: the static tags, and the build-time additions of head-meta.mjs (testnet title prefix, absolute og:url / og:image).
import { describe, expect, it } from 'vitest';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { applyHead, parseBuildNetwork, parsePublicOrigin } from '../head-meta.mjs';

const web = (rel: string) => fileURLToPath(new URL(`../${rel}`, import.meta.url));
const html = readFileSync(web('index.html'), 'utf8');
const TITLE = 'KOB – Decentralized order book on Kaspa';
const DESC = 'Buy and sell Kaspa tokens with limit orders. No admin keys, no oracle. Anyone can run an executor. Open source.';

describe('index.html head', () => {
  it('carries the title, description, Open Graph and Twitter tags, the icons and the manifest', () => {
    expect(html).toContain(`<title>${TITLE}</title>`);
    expect(html).toContain(`<meta name="description" content="${DESC}" />`);
    expect(html).toContain(`<meta property="og:title" content="${TITLE}" />`);
    expect(html).toContain(`<meta property="og:description" content="${DESC}" />`);
    expect(html).toContain('<meta property="og:type" content="website" />');
    expect(html).toContain('<meta property="og:site_name" content="KOB" />');
    expect(html).toContain('<meta name="twitter:card" content="summary_large_image" />');
    expect(html).toContain('<meta name="theme-color" content="#0f1d1c" />');
    expect(html).toContain('<link rel="icon" type="image/svg+xml" href="/favicon.svg" />');
    expect(html).toContain('<link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png" />');
    expect(html).toContain('<link rel="manifest" href="/manifest.webmanifest" />');
  });

  it('has no og:url / og:image of its own (they need an absolute URL: the build adds them from KOB_PUBLIC_URL)', () => {
    expect(html).not.toMatch(/og:url|og:image|twitter:image/);
  });

  it('every file the head and the manifest name exists in public/, and the share image is a 1200x630 PNG under 150 KB', () => {
    for (const f of ['favicon.svg', 'apple-touch-icon.png', 'icon-192.png', 'icon-512.png', 'og.png', 'manifest.webmanifest', 'robots.txt']) expect(existsSync(web(`public/${f}`)), f).toBe(true);
    const og = readFileSync(web('public/og.png'));
    expect(og.subarray(1, 4).toString('latin1')).toBe('PNG');
    expect([og.readUInt32BE(16), og.readUInt32BE(20)]).toEqual([1200, 630]);
    expect(statSync(web('public/og.png')).size).toBeLessThan(150 * 1024);
    const size = (f: string) => {
      const b = readFileSync(web(`public/${f}`));
      return [b.readUInt32BE(16), b.readUInt32BE(20)];
    };
    expect(size('apple-touch-icon.png')).toEqual([180, 180]);
    expect(size('icon-192.png')).toEqual([192, 192]);
    expect(size('icon-512.png')).toEqual([512, 512]);
  });

  it('the manifest names the app and uses the tile colour; robots.txt allows the app and keeps crawlers off the relays', () => {
    const m = JSON.parse(readFileSync(web('public/manifest.webmanifest'), 'utf8'));
    expect(m.name).toBe('KOB');
    expect(m.background_color).toBe('#0f1d1c');
    expect(m.theme_color).toBe('#0f1d1c');
    for (const i of m.icons) expect(existsSync(web(`public/${i.src}`)), i.src).toBe(true);
    const robots = readFileSync(web('public/robots.txt'), 'utf8').split('\n');
    expect(robots).toEqual(expect.arrayContaining(['User-agent: *', 'Allow: /', 'Disallow: /indexer/', 'Disallow: /node/']));
  });
});

describe('the header logo', () => {
  it("draws the favicon's four bars in its fixed colours (same rects as public/favicon.svg)", () => {
    const attrs = (s: string) =>
      [...s.matchAll(/<rect\b([^>]*?)\s*\/>/g)].map((m) => [...m[1]!.matchAll(/(\w+)="([^"]*)"/g)].map((a) => `${a[1]}=${a[2]}`).sort().join(' '));
    const fav = readFileSync(web('public/favicon.svg'), 'utf8');
    const src = readFileSync(web('src/ui/shell/Header.tsx'), 'utf8');
    const start = src.indexOf('function Logo()');
    const logo = src.slice(start, src.indexOf('</svg>', start));
    expect(logo).toContain('class="brand-mark" viewBox="0 0 512 512" aria-hidden="true" focusable="false"');
    expect(attrs(logo)).toEqual(attrs(fav));
    expect(logo).not.toMatch(/var\(--|currentColor|stroke/);
  });
});

describe('build-time head (head-meta.mjs)', () => {
  it('unset: index.html is unchanged', () => {
    expect(applyHead(html, {})).toBe(html);
    expect(applyHead(html, { network: 'mainnet', publicUrl: '' })).toBe(html);
  });

  it('a testnet build prefixes the title, og:title and twitter:title with "Testnet · "', () => {
    const out = applyHead(html, { network: 'testnet-10' });
    expect(out).toContain(`<title>Testnet · ${TITLE}</title>`);
    expect(out).toContain(`<meta property="og:title" content="Testnet · ${TITLE}" />`);
    expect(out).toContain(`<meta name="twitter:title" content="Testnet · ${TITLE}" />`);
    expect(out).toContain(`<meta name="description" content="${DESC}" />`);
  });

  it('a public URL adds absolute og:url and og:image (and twitter:image), from the origin only', () => {
    const out = applyHead(html, { publicUrl: 'https://kob.kaspa.ag/' });
    expect(out).toContain('<meta property="og:url" content="https://kob.kaspa.ag/" />');
    expect(out).toContain('<meta property="og:image" content="https://kob.kaspa.ag/og.png" />');
    expect(out).toContain('<meta name="twitter:image" content="https://kob.kaspa.ag/og.png" />');
    expect(out).toContain(`<title>${TITLE}</title>`);
    expect(out.indexOf('og:image')).toBeLessThan(out.indexOf('</head>'));
  });

  it('rejects an unknown network and a public URL that is not a plain http(s) origin', () => {
    expect(() => parseBuildNetwork('testnet')).toThrow(/KOB_NETWORK/);
    expect(parseBuildNetwork(undefined)).toBe('mainnet');
    for (const bad of ['kob.kaspa.ag', 'ftp://kob.kaspa.ag', 'https://kob.kaspa.ag/app', 'https://kob.kaspa.ag/?a=1', 'https://u:p@kob.kaspa.ag', 'javascript:alert(1)']) {
      expect(() => parsePublicOrigin(bad), bad).toThrow(/KOB_PUBLIC_URL/);
    }
    expect(parsePublicOrigin('')).toBeNull();
    expect(parsePublicOrigin('https://kob.kaspa.ag')).toBe('https://kob.kaspa.ag');
  });
});
