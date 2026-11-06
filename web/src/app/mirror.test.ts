// The GitHub Pages mirror flag (KOB_MIRROR) and the script frame guard (app/mirror.ts).
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { frameGuardOn, isFramed, MIRROR, parseMirror, privateKeySigningAllowed, showFrameGuard } from './mirror';

const src = (rel: string) => readFileSync(fileURLToPath(new URL(`../${rel}`, import.meta.url)), 'utf8');

describe('KOB_MIRROR build flag', () => {
  it('accepts only the main build (unset or empty) and github-pages; a typo fails the build', () => {
    expect(parseMirror(undefined)).toBe('');
    expect(parseMirror('')).toBe('');
    expect(parseMirror('  ')).toBe('');
    expect(parseMirror('github-pages')).toBe('github-pages');
    expect(parseMirror(' github-pages\n')).toBe('github-pages');
    for (const bad of ['gh-pages', 'GitHub-Pages', 'github', 'cloudflare', '1']) expect(() => parseMirror(bad)).toThrow(/KOB_MIRROR/);
  });

  it('the main build keeps private-key signing and has no script frame guard (its host sends frame-ancestors)', () => {
    expect(privateKeySigningAllowed('')).toBe(true);
    expect(frameGuardOn('')).toBe(false);
  });

  it('the github-pages mirror has no private-key signing and guards against framing', () => {
    expect(privateKeySigningAllowed('github-pages')).toBe(false);
    expect(frameGuardOn('github-pages')).toBe(true);
  });

  it('the bundle under test is the main build (vitest defines __KOB_MIRROR__ as ""), and the defaults follow it', () => {
    expect(MIRROR).toBe('');
    expect(privateKeySigningAllowed()).toBe(true);
    expect(frameGuardOn()).toBe(false);
  });

  it('vite.config.ts parses KOB_MIRROR and defines __KOB_MIRROR__ from it', () => {
    const cfg = src('../vite.config.ts');
    expect(cfg).toContain('parseMirror(process.env.KOB_MIRROR)');
    expect(cfg).toContain('__KOB_MIRROR__: JSON.stringify(mirror)');
  });

  it('every entry to the private key is gated: the Connect button, Settings, the issue gate, the session itself and the signer page', () => {
    expect(src('ui/shell/WalletButton.tsx')).toMatch(/const supported = keyAllowed && visibilityCheckSupported\(\)/);
    expect(src('ui/settings/SettingsView.tsx')).toMatch(/if \(!privateKeySigningAllowed\(\)\) \{[\s\S]*?settings-private-key-mirror/);
    expect(src('ui/issue/IssueView.tsx')).toContain('!privateKeySigningAllowed()');
    expect(src('app/context.tsx')).toMatch(/openPrivateKey = useCallback\(\(\) => \{\s*if \(!privateKeySigningAllowed\(\)\) return;/);
    // the signer page checks before anything else in its boot
    expect(src('signer/main.ts')).toMatch(/async function boot\(\): Promise<void> \{\s*\/\/[^\n]*\n\s*\/\/[^\n]*\n\s*if \(!privateKeySigningAllowed\(\)\) \{/);
  });
});

describe('frame guard', () => {
  it('a window is framed when its top is not itself, and when the comparison throws', () => {
    const w = {} as { top: unknown; self: unknown };
    w.top = w;
    w.self = w;
    expect(isFramed(w)).toBe(false);
    expect(isFramed({ top: {}, self: {} })).toBe(true);
    const hostile = { self: {}, get top(): unknown { throw new Error('blocked'); } };
    expect(isFramed(hostile)).toBe(true);
  });

  it('the app checks for a frame before it boots, and boots only when not framed', () => {
    const main = src('main.tsx');
    expect(main).toMatch(/if \(frameGuardOn\(\) && isFramed\(\)\) \{[\s\S]*?showFrameGuard\([\s\S]*?\} else \{\s*void boot\(\);\s*\}/);
    expect(main.match(/void boot\(\)/g)?.length).toBe(2); // the guarded start and the retry button of the boot error
  });

  it('the notice replaces everything in the root with text and one link outside the frame (plain DOM, no markup)', () => {
    type Node = { tag: string; attrs: Record<string, string>; children: Node[]; textContent?: string; [k: string]: unknown };
    const make = (tag: string): Node => {
      const n: Node = { tag, attrs: {}, children: [] };
      n.setAttribute = (k: string, v: string) => (n.attrs[k] = v);
      n.append = (...c: Node[]) => n.children.push(...c);
      return n;
    };
    const doc = { createElement: make } as unknown as Document;
    const root = make('div');
    root.children.push(make('button')); // something interactive that was there before
    root.replaceChildren = (...c: Node[]) => (root.children = c);
    showFrameGuard(doc, root as unknown as HTMLElement, 'https://rossku.github.io/kob/#/market', { title: 'T', body: 'B', link: 'L' });
    expect(root.children).toHaveLength(1);
    const box = root.children[0]!;
    expect(box.attrs['data-testid']).toBe('frame-guard');
    expect(box.children.map((c) => c.tag)).toEqual(['h1', 'p', 'a', 'p']);
    const [h, p, a, url] = box.children as [Node, Node, Node, Node];
    expect([h.textContent, p.textContent, a.textContent, url.textContent]).toEqual(['T', 'B', 'L', 'https://rossku.github.io/kob/#/market']);
    expect(a.href).toBe('https://rossku.github.io/kob/#/market');
    expect(a.target).toBe('_blank');
    expect(a.rel).toBe('noopener noreferrer');
    expect(JSON.stringify(box)).not.toMatch(/button|input|innerHTML/);
    expect(src('app/mirror.ts')).not.toMatch(/innerHTML|outerHTML|insertAdjacentHTML/);
  });
});
