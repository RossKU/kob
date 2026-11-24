// What the signer frame does with the token labels of a request (table.ts).
import { RAW_TOKEN_DECIMALS } from '../i18n';
import type { TokenHint } from './protocol';
import { tableHash, type RegistryRow } from './table';

/**
 * Turns the hints into the ones the frame may show.
 *
 * A token the table lists (the table hashing to `pinned`) takes the table's row, whatever the hint's
 * `source`. Otherwise a hint with a `source` is already marked unverified. The frame then takes the
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
    const r = listed?.get(h.covenantId);
    const inverted = h.inverted ? { inverted: true } : {};
    // a token the verified table lists takes its row whatever the hint's source (R9FB2-3): a
    // 'declared' or 'program' hint must not give a listed token other decimals
    if (r) return { covenantId: h.covenantId, ticker: r.ticker, decimals: r.decimals, ...inverted };
    if (h.source) return h;
    return { covenantId: h.covenantId, ticker: '', decimals: RAW_TOKEN_DECIMALS, source: 'assumed', ...inverted };
  });
}
