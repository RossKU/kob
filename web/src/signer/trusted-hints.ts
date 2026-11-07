// What the signer frame does with the token labels of a request (audit 2026-10-30 G7, table.ts).
import { RAW_TOKEN_DECIMALS } from '../i18n';
import type { TokenHint } from './protocol';
import { tableHash, type RegistryRow } from './table';

/**
 * The hints as the frame may show them. A hint with a `source` is already marked unverified. A registry hint (no source) is read from the
 * table when the table hashes to `pinned` and lists the token: the frame takes the table's ticker and decimals, never the hint's. Any other
 * registry hint (no verified table, a token it does not list) becomes an `assumed` hint: no ticker and no decimals, as any token nothing states the
 * decimals of is shown (an order of the transaction names its own scale: summary.ts `withOrderScales`). The decimals the app names are not used for it.
 */
export function trustedHints(hints: readonly TokenHint[], rows: readonly RegistryRow[] | undefined, pinned: string | undefined): TokenHint[] {
  const listed = rows && pinned && tableHash(rows) === pinned ? new Map(rows.map((r) => [r.covenantId, r])) : null;
  return hints.map((h) => {
    if (h.source) return h;
    const r = listed?.get(h.covenantId);
    const inverted = h.inverted ? { inverted: true } : {};
    if (r) return { covenantId: h.covenantId, ticker: r.ticker, decimals: r.decimals, ...inverted };
    return { covenantId: h.covenantId, ticker: '', decimals: RAW_TOKEN_DECIMALS, source: 'assumed', ...inverted };
  });
}
