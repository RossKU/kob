// The build's hosting mirror (build-time flag `KOB_MIRROR`, vite.config.ts `define: __KOB_MIRROR__`; docs/ops/web-hosting.md, "GitHub Pages mirror").
//
//   ''              the main build (Cloudflare Pages, the soak host, `vite preview`): unchanged. Its host sends public/_headers, whose CSP has
//                   `frame-ancestors 'none'` and the signer page's own `sandbox` / `frame-ancestors 'self'`.
//   'github-pages'  the fallback mirror at https://rossku.github.io/kob/. GitHub Pages sends no custom headers, so the page has only the <meta> CSP
//                   (csp.mjs; a <meta> cannot carry frame-ancestors or sandbox). Hence, in this build only:
//                     * a script frame guard: a page inside a frame shows a short notice and a link to open it on its own, nothing interactive;
//                     * no private-key signing: the signer page relies on its header policy (sandbox, frame-ancestors 'self'), which this host
//                       cannot send, so the option is not offered at all (wallet extensions only) and signer.html never starts its key entry.
export type Mirror = '' | 'github-pages';

/** The mirrors a build may be made for (KOB_MIRROR); anything else fails the build (vite.config.ts). */
export const MIRRORS: readonly Mirror[] = ['', 'github-pages'];

/** The KOB_MIRROR value of a build; throws on an unknown one (a typo must not silently build the main variant). */
export function parseMirror(raw: string | undefined): Mirror {
  const v = (raw ?? '').trim();
  if ((MIRRORS as readonly string[]).includes(v)) return v as Mirror;
  throw new Error(`KOB_MIRROR must be empty or github-pages, got "${v}"`);
}

/** The mirror this bundle was built for. */
export const MIRROR: Mirror = typeof __KOB_MIRROR__ === 'string' && __KOB_MIRROR__ === 'github-pages' ? 'github-pages' : '';

/** Whether the private-key signer may be offered: only where the host sends the signer page's header policy. */
export const privateKeySigningAllowed = (mirror: Mirror = MIRROR): boolean => mirror === '';

/** Whether the page guards against being framed by script (a mirror whose host cannot send frame-ancestors). */
export const frameGuardOn = (mirror: Mirror = MIRROR): boolean => mirror !== '';

/** Whether this window is inside a frame. A browser that refuses the comparison counts as framed. */
export function isFramed(w: { top: unknown; self: unknown } = window): boolean {
  try {
    return w.top !== w.self;
  } catch {
    return true;
  }
}

export interface FrameGuardText {
  title: string;
  body: string;
  link: string;
}

/**
 * Replaces the content of `root` with the frame notice: a heading, one sentence and a link that opens `href` in a new browsing context (outside
 * the frame). Plain DOM (createElement, textContent): no markup reaches a sink, so it holds under Trusted Types.
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
