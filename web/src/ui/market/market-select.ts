// One market page, one pair selector. The header shows the pair as `BASE / QUOTE`, and each side
// is a drop-down of assets (KAS and the tokens). This file is the pure part. It decides which pair
// a market route displays, what a changed selector leads to, and which assets a side may offer. It
// is display only. The market routes are `#/market/<token>`, the token's KAS book shown TOKEN/KAS
// or flipped KAS/TOKEN, and `#/market/<base>/<quote>`, a token pair.
import { pairRoute, tokenRoute, type Route } from '../../app/router';
import { filterTokens, type TokenRow } from './token-model';

export const KAS = 'kas';
/** `kas` or a token's covenant id in lower case. */
export type AssetId = string;

export interface DisplayedPair {
  left: AssetId;
  right: AssetId;
}

/**
 * The pair a market shows: `TOKEN/KAS` natively, `KAS/TOKEN` inverted, and a token pair as routed
 * (BASE/QUOTE, where its swap is a route).
 */
export function displayedPair(base: string, quote: string | null, inverted: boolean): DisplayedPair {
  const b = base.toLowerCase();
  if (quote !== null) return { left: b, right: quote.toLowerCase() };
  return inverted ? { left: KAS, right: b } : { left: b, right: KAS };
}

/** A selector changed, giving the new pair. Picking the asset on the other side swaps the sides. */
export function changePair(cur: DisplayedPair, side: 'left' | 'right', next: AssetId): DisplayedPair {
  const a = next.toLowerCase();
  const other = side === 'left' ? cur.right : cur.left;
  if (a === other) return { left: cur.right, right: cur.left };
  return side === 'left' ? { left: a, right: cur.right } : { left: cur.left, right: a };
}

/**
 * Where a pair leads. It is the token's own KAS market, with the orientation to remember (`true`
 * means KAS/TOKEN), or the token pair. KAS/KAS is no market and gives null.
 */
export function marketFor(p: DisplayedPair): { route: Route; inverted: boolean | null } | null {
  if (p.left === KAS && p.right === KAS) return null;
  if (p.right === KAS) return { route: tokenRoute(p.left), inverted: false };
  if (p.left === KAS) return { route: tokenRoute(p.right), inverted: true };
  return { route: pairRoute(p.left, p.right), inverted: null };
}

export interface AssetChoice {
  /** `kas` or the covenant id. */
  value: AssetId;
  /** The drop-down entry. For tokens it is the ticker and short id. */
  label: string;
  /** What the closed selector shows. */
  short: string;
}

/**
 * The assets one side offers. They are KAS and every token of `rows`, which are the tokens this
 * build can trade. Any two of them make a pair, registered or not. The asset on the other side is
 * never offered, and the flip control swaps them.
 */
export function assetChoices<T extends { covenantId: string; ticker: string }>(
  rows: readonly T[], other: AssetId, label: (r: T) => string,
): AssetChoice[] {
  const o = other.toLowerCase();
  const out: AssetChoice[] = [];
  if (o !== KAS) out.push({ value: KAS, label: 'KAS', short: 'KAS' });
  for (const r of rows) {
    const id = r.covenantId.toLowerCase();
    if (id === o) continue;
    out.push({ value: id, label: label(r), short: r.ticker });
  }
  return out;
}

/**
 * The second line of a picker's row: the token's name. It is empty when there is none or it only
 * repeats the ticker (KASPERO / Kaspero).
 */
export const pickerName = (name: string | null | undefined, ticker: string): string => (name && name.trim().toLowerCase() !== ticker.trim().toLowerCase() ? name : '');

/** The most choices a picker lists at once. The search narrows the rest. */
export const PICKER_ROWS = 50;

/**
 * The choices a picker's search leaves. Tokens match by ticker, name or covenant id
 * (`filterTokens`, over `rows`) and KAS by its name. An empty search leaves all.
 */
export function filterChoices(choices: readonly AssetChoice[], rows: readonly TokenRow[], query: string): AssetChoice[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...choices];
  const hits = new Set(filterTokens(rows, q).map((r) => r.covenantId.toLowerCase()));
  return choices.filter((c) => (c.value === KAS ? KAS.includes(q) : hits.has(c.value)));
}
