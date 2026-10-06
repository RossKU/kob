// One market page, one pair selector: the header shows the pair as `BASE / QUOTE` and each side is a drop-down of assets (KAS and the tokens). This file is the
// pure part: which pair a market route displays, what a changed selector leads to, and which assets a side may offer. Display only: the market routes
// are the same as before (`#/market/<token>` = the token's KAS book, shown TOKEN/KAS or flipped KAS/TOKEN; `#/market/<base>/<quote>` = a token pair).
import { pairRoute, tokenRoute, type Route } from '../../app/router';

export const KAS = 'kas';
/** `kas` or a token's covenant id (lower case). */
export type AssetId = string;

export interface DisplayedPair {
  left: AssetId;
  right: AssetId;
}

/** The pair a market shows: `TOKEN/KAS` natively, `KAS/TOKEN` inverted; a token pair as routed (BASE/QUOTE; its swap is a route). */
export function displayedPair(base: string, quote: string | null, inverted: boolean): DisplayedPair {
  const b = base.toLowerCase();
  if (quote !== null) return { left: b, right: quote.toLowerCase() };
  return inverted ? { left: KAS, right: b } : { left: b, right: KAS };
}

/** A selector changed: the new pair. Picking the asset that sits on the other side swaps the two sides. */
export function changePair(cur: DisplayedPair, side: 'left' | 'right', next: AssetId): DisplayedPair {
  const a = next.toLowerCase();
  const other = side === 'left' ? cur.right : cur.left;
  if (a === other) return { left: cur.right, right: cur.left };
  return side === 'left' ? { left: a, right: cur.right } : { left: cur.left, right: a };
}

/**
 * Where a pair leads: the token's own KAS market (with the orientation to remember: `true` = KAS/TOKEN) or the token pair. KAS/KAS is no market (null).
 */
export function marketFor(p: DisplayedPair): { route: Route; inverted: boolean | null } | null {
  if (p.left === KAS && p.right === KAS) return null;
  if (p.right === KAS) return { route: tokenRoute(p.left), inverted: false };
  if (p.left === KAS) return { route: tokenRoute(p.right), inverted: true };
  return { route: pairRoute(p.left, p.right), inverted: null };
}

export interface AssetChoice {
  /** `kas` or the covenant id */
  value: AssetId;
  /** the drop-down entry (tokens: ticker, short id and standing) */
  label: string;
  /** what the closed selector shows */
  short: string;
}

/**
 * The assets one side offers. Next to KAS every known token is offered (the token's KAS market); next to a token only KAS and tokens that can be paired
 * (in the registry: `pairable`). The asset on the other side is never offered (swap with the flip control).
 */
export function assetChoices<T extends { covenantId: string; ticker: string; pairable: boolean }>(
  rows: readonly T[], other: AssetId, label: (r: T) => string,
): AssetChoice[] {
  const o = other.toLowerCase();
  const out: AssetChoice[] = [];
  if (o !== KAS) out.push({ value: KAS, label: 'KAS', short: 'KAS' });
  for (const r of rows) {
    const id = r.covenantId.toLowerCase();
    if (id === o) continue;
    if (o !== KAS && !r.pairable) continue;
    out.push({ value: id, label: label(r), short: r.ticker });
  }
  return out;
}
