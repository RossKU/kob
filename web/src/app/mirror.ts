// The hosting mirror of this build. The build-time flag `KOB_MIRROR` reaches the bundle as
// `__KOB_MIRROR__` (vite.config.ts `define`). See docs/ops/web-hosting.md, "GitHub Pages mirror".
//
// - '' is the main build (Cloudflare Pages, the soak host, `vite preview`). Its host sends
//   public/_headers, whose CSP has `frame-ancestors 'none'` and gives the signer page its own
//   `sandbox` and `frame-ancestors 'self'`.
// - 'github-pages' is the fallback mirror at https://rossku.github.io/kob/. That host sends no
//   custom headers, so the page has only the <meta> CSP (csp.mjs), which cannot carry
//   `frame-ancestors` or `sandbox`. This build therefore differs in two ways.
//   - A script frame guard shows a short notice and a link instead of the page when framed.
//   - Private-key signing is not offered, because the signer page relies on header policy this
//     host cannot send. Wallet extensions only, and signer.html never starts its key entry.

/** The hosting mirror a build is made for: the main build ('') or the GitHub Pages mirror. */
export type Mirror = '' | 'github-pages';

/** The mirrors a build may be made for (KOB_MIRROR). Anything else fails the build. */
export const MIRRORS: readonly Mirror[] = ['', 'github-pages'];

/**
 * The KOB_MIRROR value of a build. Throws on an unknown one, so a typo cannot silently build the
 * main variant.
 */
export function parseMirror(raw: string | undefined): Mirror {
  const v = (raw ?? '').trim();
  if ((MIRRORS as readonly string[]).includes(v)) return v as Mirror;
  throw new Error(`KOB_MIRROR must be empty or github-pages, got "${v}"`);
}

/** The mirror this bundle was built for. */
export const MIRROR: Mirror = typeof __KOB_MIRROR__ === 'string' && __KOB_MIRROR__ === 'github-pages' ? 'github-pages' : '';

/** Whether the private-key signer may be offered. Only the main build's host sends its headers. */
export const privateKeySigningAllowed = (mirror: Mirror = MIRROR): boolean => mirror === '';

/** Whether the page guards against being framed by script, for hosts without frame-ancestors. */
export const frameGuardOn = (mirror: Mirror = MIRROR): boolean => mirror !== '';

/** Whether this window is inside a frame. A browser that refuses the comparison counts as one. */
export function isFramed(w: { top: unknown; self: unknown } = window): boolean {
  try {
    return w.top !== w.self;
  } catch {
    return true;
  }
}

/** The texts of the frame notice. */
export interface FrameGuardText {
  title: string;
  body: string;
  link: string;
}

/**
 * Replaces the content of `root` with the frame notice. It has a heading, one sentence and a link
 * that opens `href` outside the frame. Plain DOM only (createElement, textContent), so no markup
 * reaches a sink and it holds under Trusted Types.
 */
export function showFrameGuard(doc: Document, root: HTMLElement, href: string, text: FrameGuardText): void {
  const box = doc.createElement('div');
  box.className = 'frame-guard';
  box.setAttribute('data-testid', 'frame-guard');
  const h = doc.createElement('h1');
  h.textContent = text.title;
  const p = doc.createElement('p');
  p.textContent = text.body;
  const a = doc.createElement('a');
  a.href = href;
  a.target = '_blank';
  a.rel = 'noopener noreferrer';
  a.textContent = text.link;
  a.setAttribute('data-testid', 'frame-guard-link');
  const url = doc.createElement('p');
  url.className = 'frame-guard-url';
  url.textContent = href;
  box.append(h, p, a, url);
  root.replaceChildren(box);
}
