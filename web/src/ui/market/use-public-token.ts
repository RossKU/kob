// A KCC-20 token that neither the registry nor the indexer's token list names (no order of it was ever placed), opened WITHOUT a wallet: the indexer names
// token outputs of it (`GET /v1/token-utxos?token=`) and the node confirms each one (`kob/open-token.ts` `findPublicToken`). The confirmed outputs' addresses
// commit to the token program and the extension commitment, which is all a first order needs besides the decimals. Asked only after the lists and the
// wallet's own holdings (`use-held-token.ts`) found nothing.
import { useMemo } from 'preact/hooks';
import { useServices } from '../../app/context';
import { findPublicToken, type PublicTokenIssue } from '../../kob/open-token';
import type { Hex } from '../../kob/types';
import { useAsync } from '../kit';
import { unknownRow, type TokenRow } from './token-model';

export interface PublicTokenState {
  row: TokenRow | null;
  /** which fact is missing (null while loading, when not asked, or with a row) */
  reason: PublicTokenIssue | null;
  loading: boolean;
}

/** `active`: the token is in neither list and the wallet's holdings do not make it tradable. */
export function usePublicToken(covenantId: Hex, active: boolean): PublicTokenState {
  const { kob, registry, tracker, config } = useServices();
  const id = covenantId.toLowerCase();
  const found = useAsync(async () => (active ? findPublicToken(tracker, kob, id, config.network) : null), [active, id, tracker, kob, config.network]);
  return useMemo((): PublicTokenState => {
    // `useAsync` keeps the last good answer while it reloads: an answer about another token is not one
    const r = found.data && (!found.data.ok || found.data.view.covenant_id === id) ? found.data : null;
    if (!r) return { row: null, reason: null, loading: active && found.loading };
    return r.ok ? { row: unknownRow(registry, r.view, 'public'), reason: null, loading: false } : { row: null, reason: r.reason, loading: false };
  }, [found.data, found.loading, registry, active, id]);
}
