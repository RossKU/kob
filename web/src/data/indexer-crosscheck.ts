// Cross-checking indexers.
//
// KOB is open source, so anyone can run an indexer, including a dishonest one. One indexer is
// never trusted blindly when more than one is configured (config `extraIndexerUrls`). The key
// facts a UI decision rests on are read from each indexer and compared:
// - the token's powers,
// - the scale of an open-list token's market (`market_scale`), which sets the price scale of new
//   orders,
// - the best ask and bid of the book.
// A disagreement is surfaced as a warning. Nothing is silently averaged or "voted".
import type { Hex } from '../kob/types';
import type { IndexerApi } from './indexer';

/** The key facts of one token as one indexer reports them. */
export interface IndexerFacts {
  /**
   * The token as this indexer lists it, null when not listed. `marketScale` is undefined when the
   * indexer does not report it.
   */
  token: { powers: string[]; marketScale?: number | null } | null;
  bestAsk: bigint | null;
  bestBid: bigint | null;
}

/** What a cross-check found. */
export type CrossCheckCode = 'powers-differ' | 'market-scale-differs' | 'best-ask-differs' | 'best-bid-differs' | 'token-missing' | 'unreachable';

/** One disagreement or failure of a verifier, with both values where there are two. */
export interface CrossCheckFinding {
  code: CrossCheckCode;
  /** The label (URL) of the indexer that disagrees or failed. */
  other: string;
  primary?: string;
  otherValue?: string;
}

/**
 * A best price is "different" when the two prices are further apart than this, in bps of the
 * larger. Book lag alone must not raise the alarm.
 */
export const PRICE_TOLERANCE_BPS = 200n;

const same = (a: readonly string[], b: readonly string[]): boolean => a.length === b.length && [...a].sort().join(',') === [...b].sort().join(',');
const priceApart = (a: bigint, b: bigint): boolean => {
  const hi = a > b ? a : b;
  const diff = a > b ? a - b : b - a;
  return hi > 0n && (diff * 10_000n) / hi > PRICE_TOLERANCE_BPS;
};

/** Pure comparison of the facts of two indexers. */
export function compareFacts(primary: IndexerFacts, other: IndexerFacts, label: string): CrossCheckFinding[] {
  const out: CrossCheckFinding[] = [];
  if (primary.token && !other.token) out.push({ code: 'token-missing', other: label });
  else if (primary.token && other.token) {
    if (!same(primary.token.powers, other.token.powers)) out.push({ code: 'powers-differ', other: label, primary: primary.token.powers.join(',') || '-', otherValue: other.token.powers.join(',') || '-' });
    const [ma, mb] = [primary.token.marketScale, other.token.marketScale];
    if (ma !== undefined && mb !== undefined && ma !== mb) out.push({ code: 'market-scale-differs', other: label, primary: ma === null ? '-' : String(ma), otherValue: mb === null ? '-' : String(mb) });
  }
  const px = (a: bigint | null) => (a === null ? '-' : a.toString());
  for (const [code, a, b] of [['best-ask-differs', primary.bestAsk, other.bestAsk], ['best-bid-differs', primary.bestBid, other.bestBid]] as const) {
    if ((a === null) !== (b === null) || (a !== null && b !== null && priceApart(a, b))) out.push({ code, other: label, primary: px(a), otherValue: px(b) });
  }
  return out;
}

const priceOf = (l: { price: string } | undefined): bigint | null => (l && /^\d+$/.test(l.price) ? BigInt(l.price) : null);

/** Reads the facts of one token from one indexer. */
export async function readFacts(api: Pick<IndexerApi, 'tokens' | 'book'>, covenantId: Hex, signal?: AbortSignal): Promise<IndexerFacts> {
  const o = signal ? { signal } : undefined;
  const [tokens, book] = await Promise.all([api.tokens(o), api.book(covenantId, { depth: 1, aggregate: true }, o)]);
  const t = tokens.find((x) => x.covenant_id === covenantId);
  return {
    token: t
      ? { powers: Array.isArray(t.powers) ? t.powers : [], ...(t.market_scale !== undefined ? { marketScale: t.market_scale } : {}) }
      : null,
    bestAsk: priceOf(book.asks[0]),
    bestBid: priceOf(book.bids[0]),
  };
}

/** The findings of a cross-check and how many verifiers answered. */
export interface CrossCheckResult {
  /** Verifiers that answered. */
  checked: number;
  findings: CrossCheckFinding[];
}

/**
 * Compares the primary indexer with every verifier. An unreachable verifier is reported
 * (`unreachable`), never counted as agreement.
 */
export async function crossCheckIndexers(
  primary: Pick<IndexerApi, 'tokens' | 'book'>,
  verifiers: readonly { label: string; api: Pick<IndexerApi, 'tokens' | 'book'> }[],
  covenantId: Hex,
  signal?: AbortSignal,
): Promise<CrossCheckResult> {
  if (!verifiers.length) return { checked: 0, findings: [] };
  const base = await readFacts(primary, covenantId, signal);
  const findings: CrossCheckFinding[] = [];
  let checked = 0;
  await Promise.all(
    verifiers.map(async (v) => {
      try {
        const f = await readFacts(v.api, covenantId, signal);
        checked++;
        findings.push(...compareFacts(base, f, v.label));
      } catch {
        findings.push({ code: 'unreachable', other: v.label });
      }
    }),
  );
  return { checked, findings };
}

/** How many independent indexers back the data shown. */
export type IndexerTrust = 'none' | 'single' | 'multi';

/**
 * How many independent indexers back the data shown: none, one (unverified) or several
 * (cross-checked).
 */
export const indexerTrust = (hasPrimary: boolean, verifiers: number): IndexerTrust => (!hasPrimary ? 'none' : verifiers > 0 ? 'multi' : 'single');

/**
 * The best ask and bid of a market as each verifier reports it. They are references for the start
 * of a market order that do not come from the primary indexer. `toBook` turns an indexer book
 * into the planner's units, as `bookFromIndexer` does. A verifier that fails is left out.
 */
export async function readReferenceTouches<B extends { asks: { price: bigint }[]; bids: { price: bigint }[] }>(
  verifiers: readonly { label: string; api: Pick<IndexerApi, 'book'> }[],
  covenantId: Hex,
  toBook: (view: Awaited<ReturnType<IndexerApi['book']>>) => B,
  signal?: AbortSignal,
): Promise<{ label: string; bestAsk: bigint | null; bestBid: bigint | null }[]> {
  const o = signal ? { signal } : undefined;
  const out = await Promise.all(
    verifiers.map(async (v): Promise<{ label: string; bestAsk: bigint | null; bestBid: bigint | null } | null> => {
      try {
        const b = toBook(await v.api.book(covenantId, { depth: 20, aggregate: true }, o));
        return { label: v.label, bestAsk: b.asks[0]?.price ?? null, bestBid: b.bids[0]?.price ?? null };
      } catch {
        return null;
      }
    }),
  );
  return out.filter((x): x is { label: string; bestAsk: bigint | null; bestBid: bigint | null } => x !== null);
}

/**
 * The references further indexers give for a market start. They are the touches of those that
 * answered, and the labels of those that did not.
 */
export interface References {
  touches: { label: string; bestAsk: bigint | null; bestBid: bigint | null }[];
  unavailable: string[];
}

type Touch = { bestAsk: bigint | null; bestBid: bigint | null };

/**
 * Reads `read` from every verifier. A verifier that fails, or answers no book for a market another
 * one has, is reported in `unavailable` and never left out silently.
 */
async function collect(
  verifiers: readonly { label: string; api: Pick<IndexerApi, 'book'> }[],
  read: (api: Pick<IndexerApi, 'book'>) => Promise<Touch>,
): Promise<References> {
  const out = await Promise.all(verifiers.map(async (v) => ({ label: v.label, touch: await read(v.api).catch(() => null) })));
  return {
    touches: out.filter((x) => x.touch !== null).map((x) => ({ label: x.label, ...(x.touch as Touch) })),
    unavailable: out.filter((x) => x.touch === null).map((x) => x.label),
  };
}

/** The best ask and bid of a KAS market per verifier, and the verifiers that did not answer. */
export function readReferences<B extends { asks: { price: bigint }[]; bids: { price: bigint }[] }>(
  verifiers: readonly { label: string; api: Pick<IndexerApi, 'book'> }[],
  covenantId: Hex,
  toBook: (view: Awaited<ReturnType<IndexerApi['book']>>) => B,
  signal?: AbortSignal,
): Promise<References> {
  const o = signal ? { signal } : undefined;
  return collect(verifiers, async (api) => {
    const b = toBook(await api.book(covenantId, { depth: 20, aggregate: true }, o));
    return { bestAsk: b.asks[0]?.price ?? null, bestBid: b.bids[0]?.price ?? null };
  });
}

/**
 * The touches of a pair A/B, in B base units per whole A, that each verifier's two KAS books
 * imply (`impliedPairRate`). Buying A with B starts at A's ask over B's bid. Selling A for B
 * starts at A's bid over B's ask. A verifier that does not answer for both books is reported
 * unavailable.
 */
export function readPairReferences<B extends { asks: { price: bigint }[]; bids: { price: bigint }[] }>(
  verifiers: readonly { label: string; api: Pick<IndexerApi, 'book'> }[],
  a: { covenantId: Hex; toBook: (view: Awaited<ReturnType<IndexerApi['book']>>) => B },
  b: { covenantId: Hex; toBook: (view: Awaited<ReturnType<IndexerApi['book']>>) => B; scale: bigint },
  signal?: AbortSignal,
): Promise<References> {
  const o = signal ? { signal } : undefined;
  const rate = (kasA: bigint | undefined, kasB: bigint | undefined): bigint | null =>
    kasA === undefined || kasB === undefined || kasA <= 0n || kasB <= 0n || b.scale <= 0n ? null : (kasA * b.scale) / kasB;
  return collect(verifiers, async (api) => {
    const [ba, bb] = await Promise.all([api.book(a.covenantId, { depth: 20, aggregate: true }, o), api.book(b.covenantId, { depth: 20, aggregate: true }, o)]);
    const A = a.toBook(ba);
    const Bk = b.toBook(bb);
    return { bestAsk: rate(A.asks[0]?.price, Bk.bids[0]?.price), bestBid: rate(A.bids[0]?.price, Bk.asks[0]?.price) };
  });
}
