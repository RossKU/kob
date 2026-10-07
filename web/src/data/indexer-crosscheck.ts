// Cross-checking indexers. KOB is open source: anyone can run an indexer, including a dishonest one, so one indexer is never trusted blindly when more
// than one is configured (config `extraIndexerUrls`). The key facts a UI decision rests on are read from each indexer and compared: the token's standing
// and powers, and the best ask / bid of the book. A disagreement is surfaced as a warning; nothing is silently averaged or "voted".
import type { Hex } from '../kob/types';
import type { IndexerApi } from './indexer';

export interface IndexerFacts {
  /** the token as this indexer lists it (null: not listed) */
  token: { standing: string | null; powers: string[] } | null;
  bestAsk: bigint | null;
  bestBid: bigint | null;
}

export type CrossCheckCode = 'standing-differs' | 'powers-differ' | 'best-ask-differs' | 'best-bid-differs' | 'token-missing' | 'unreachable';

export interface CrossCheckFinding {
  code: CrossCheckCode;
  /** label (URL) of the indexer that disagrees / failed */
  other: string;
  primary?: string;
  otherValue?: string;
}

/** A best price is "different" when the two prices are further apart than this (bps of the larger): book lag alone must not raise the alarm. */
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
    if ((primary.token.standing ?? '') !== (other.token.standing ?? '')) out.push({ code: 'standing-differs', other: label, primary: primary.token.standing ?? '-', otherValue: other.token.standing ?? '-' });
    if (!same(primary.token.powers, other.token.powers)) out.push({ code: 'powers-differ', other: label, primary: primary.token.powers.join(',') || '-', otherValue: other.token.powers.join(',') || '-' });
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
    token: t ? { standing: t.standing ?? null, powers: Array.isArray(t.powers) ? t.powers : [] } : null,
    bestAsk: priceOf(book.asks[0]),
    bestBid: priceOf(book.bids[0]),
  };
}

export interface CrossCheckResult {
  /** verifiers that answered */
  checked: number;
  findings: CrossCheckFinding[];
}

/** Compares the primary indexer with every verifier. An unreachable verifier is reported (`unreachable`), never counted as agreement. */
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

export type IndexerTrust = 'none' | 'single' | 'multi';

/** How many independent indexers back the data shown: none, one (unverified) or several (cross-checked). */
export const indexerTrust = (hasPrimary: boolean, verifiers: number): IndexerTrust => (!hasPrimary ? 'none' : verifiers > 0 ? 'multi' : 'single');

/**
 * The best ask / bid of a market as each verifier reports it (`toBook` turns an indexer book into the planner's units, e.g. `bookFromIndexer`):
 * references for the start of a market order that do not come from the primary indexer. A verifier that fails is left out.
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
