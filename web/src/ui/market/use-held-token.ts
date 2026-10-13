// A KCC-20 token that neither the registry nor the indexer lists (no order of it was ever placed): its market page is offered only from the
// wallet's own token UTXOs of it that the node confirms (`kob/open-token.ts` `findHeldToken`, `heldTokenView`). The program candidates are the
// ones this browser stores (an issuance made here, transfers built here, an import) and those of the wallet's node-confirmed holdings (a token
// bought elsewhere); the node check is `TokenTracker.tokenUtxosFor`.
import { useMemo } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { findHeldToken, type HeldTokenIssue } from '../../kob/open-token';
import type { Hex } from '../../kob/types';
import { useAsync } from '../kit';
import { unknownRow, type TokenRow } from './token-model';

export interface HeldTokenState {
  row: TokenRow | null;
  /** why the wallet's holdings do not make the token tradable here (null while loading, without a wallet, or with a row) */
  reason: HeldTokenIssue | null;
  loading: boolean;
}

/** `active`: the token is in neither list (the lists have loaded). */
export function useHeldToken(covenantId: Hex, active: boolean): HeldTokenState {
  const { kob, registry, tracker, config } = useServices();
  const pubkey = useWallet().info?.pubkey ?? null;
  const id = covenantId.toLowerCase();
  const held = useAsync(
    async () => {
      if (!active || !pubkey) return null;
      return findHeldToken(tracker, kob, pubkey, id, config.network);
    },
    [active, pubkey, id, tracker, kob, config.network],
  );
  return useMemo((): HeldTokenState => {
    const r = held.data;
    if (!r) return { row: null, reason: null, loading: active && !!pubkey && held.loading };
    return r.ok ? { row: unknownRow(registry, r.view, 'held'), reason: null, loading: false } : { row: null, reason: r.reason, loading: false };
  }, [held.data, held.loading, registry, active, pubkey]);
}
