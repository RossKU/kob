// What the signer frame does with the token labels of a request (table.ts).
import { RAW_TOKEN_DECIMALS } from '../i18n';
import type { TokenHint } from './protocol';
import { tableHash, type RegistryRow } from './table';

/**
 * Turns the hints into the ones the frame may show.
 *
 * A hint with a `source` is already marked unverified. A registry hint, one with no source, is read
 * from the table when the table hashes to `pinned` and lists the token. The frame then takes the
 * table's ticker and decimals, never the hint's.
 *
 * Any other registry hint becomes an `assumed` hint, with no ticker and no decimals. That covers no
 * verified table and a token the table does not list. Such a token is shown like any token whose
 * decimals nothing states. An order of the transaction can still name its own scale (summary.ts
 * `withOrderScales`). The decimals the app names are not used for it.
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
