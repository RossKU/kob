// What the signer page has really shown of a request before its Approve button may be used (no DOM here: signer/main.ts feeds it the
// IntersectionObserver v2 entries, unit tests feed it directly).
//
// The frame's visibility check alone looks at the Approve button: a page that frames the signer can clip the frame (a small box with
// `overflow: hidden`, the frame moved up inside it) so that only the buttons show, and draw its own summary above them. So every part of the
// summary (the title, each line, each order's title, token and terms) must have been fully on screen and unoccluded for `minMs` at some time
// since the request appeared, and the buttons must be so now. A long summary is read by scrolling the frame; nothing the app does can mark a
// part as seen without showing it.

/**
 * Whether this browser reports real visibility (IntersectionObserver v2, Chromium). Without it the frame cannot tell a clipped or covered
 * frame from a shown one, so the private-key signer is not offered and never approves there.
 */
export function visibilityCheckSupported(g: { IntersectionObserver?: unknown; IntersectionObserverEntry?: unknown } = globalThis): boolean {
  const proto = (g.IntersectionObserverEntry as { prototype?: unknown } | undefined)?.prototype;
  return typeof g.IntersectionObserver === 'function' && typeof proto === 'object' && proto !== null && 'isVisible' in proto;
}

/** How long a part must stay fully visible to count as seen. */
export const SEEN_MIN_MS = 400;

export class SeenTracker {
  private readonly since = new Map<string, number | null>();
  private readonly seen = new Set<string>();
  private buttonsSince: number | null = null;

  constructor(parts: readonly string[], private readonly minMs = SEEN_MIN_MS) {
    for (const p of parts) this.since.set(p, null);
  }

  /** A part became fully visible (`true`) or stopped being so (`false`) at `now`. Unknown parts are ignored. */
  part(id: string, fully: boolean, now: number): void {
    if (!this.since.has(id)) return;
    const s = this.since.get(id)!;
    if (fully) {
      if (s === null) this.since.set(id, now);
    } else {
      if (s !== null && now - s >= this.minMs) this.seen.add(id);
      this.since.set(id, null);
    }
  }

  /** The buttons became fully visible or stopped being so. */
  buttons(fully: boolean, now: number): void {
    if (fully) this.buttonsSince ??= now;
    else this.buttonsSince = null;
  }

  private partSeen(id: string, now: number): boolean {
    if (this.seen.has(id)) return true;
    const s = this.since.get(id);
    return s !== null && s !== undefined && now - s >= this.minMs;
  }

  /** Every part has been seen and the buttons are visible now (for `minMs`). */
  ready(now: number): boolean {
    if (this.buttonsSince === null || now - this.buttonsSince < this.minMs) return false;
    for (const id of this.since.keys()) if (!this.partSeen(id, now)) return false;
    return true;
  }

  /** The parts not seen yet (tests, diagnostics). */
  missing(now: number): string[] {
    return [...this.since.keys()].filter((id) => !this.partSeen(id, now));
  }
}
